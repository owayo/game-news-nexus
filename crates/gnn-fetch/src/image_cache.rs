//! 本文の画像を保存しておくディスクキャッシュ。
//!
//! 一度表示した画像はオフラインでも出せるようにし、同じ画像を何度も取りに行かない。
//!
//! 鍵は URL (フラグメントは外す)。問い合わせは推測で外さない (`?v=2` は中身が変わったときの版の
//! ことが多く、`?t=` は署名の期限のこともある)。取得のたびに問い合わせが変わる画像は、取得の定義の
//! `url_identity.images` で宣言し、保存した本文と同じ参照 (`https://source-image.invalid/...`) を鍵にする。
//!
//! 配信元の検証子 (ETag・Last-Modified) は、画像のファイルの横 (`<名前>.meta`) に置く。検証子はキャッシュに
//! あるバイト列を説明するものなので、画像と一緒に消す (DB に置くと、キャッシュを消したときに食い違う)。
//! 画像のファイルの更新時刻は、最後に配信元で確かめた時刻 (取ってきた・304 で変わっていないと分かった) にする。
//! 古いものから消す [`ImageCache::prune`] も、この時刻で並べる (読んだだけでは時刻を進めない)。
//!
//! `.meta` には、書いたときの長さと SHA-256 ([`Integrity`]) も入れる (検証子がなくても書く)。読むときに照らし
//! ([`ImageCache::read_checked`])、書いたあとで壊れた画像 (電源が落ちて書ききれなかったなど) を見つける。
//! 照らすものがない画像 (これを入れる前の版で書いたもの。一時ファイルの名前が PID だけだった頃に、同時に書いて
//! 混ざったものもありうる) は「確かめていない」として分け、配信元で確かめ直すときは検証子を使わずに取り直して
//! 比べる (304 で延命しない)。画面は、確かめていない画像を gpui が読めなかったときに捨てて取り直す。

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use anyhow::{Context as _, Result};
use parking_lot::Mutex;
use reqwest::StatusCode;
use reqwest::header::{
    CONTENT_TYPE, ETAG, HeaderMap, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED, RETRY_AFTER,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use url::Url;

/// 検証子のファイルの拡張子。
const META_EXTENSION: &str = "meta";
/// 書きかけのファイルの拡張子。
const TEMP_EXTENSION: &str = "tmp";
/// 書きかけのまま残ったファイル (途中で止まった書き込み) を消すまでの時間。
const TEMP_KEEP: Duration = Duration::from_secs(3600);

/// 書きかけのファイルの名前を、同じプロセスの中でも重ならないようにする通し番号。
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct ImageCache {
    dir: PathBuf,
    /// 画像ごとの書き込みの世代 (このプロセスの中だけ)。取りに行っているあいだにほかが入れ直していたら、
    /// 遅れて届いた結果で上書きしない。画像と検証子の組の書き込みと prune の削除は、この鍵を取って行う。
    generations: Arc<Mutex<HashMap<PathBuf, u64>>>,
    /// テストで、読み手と書き手の食い違いを作る止め場所 ([`ReadPause`])。
    #[cfg(test)]
    read_pause: Arc<ReadPause>,
}

/// テストで、`.meta` を読んだあと画像を読む前に一度だけ呼ぶ処理 (書き手を割り込ませる)。
#[cfg(test)]
#[derive(Default)]
struct ReadPause(Mutex<Option<Box<dyn FnOnce() + Send>>>);

#[cfg(test)]
impl std::fmt::Debug for ReadPause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReadPause")
    }
}

/// 配信元が返した検証子。キャッシュの画像を条件付きの取得で確かめ直すときに、そのまま送る
/// (ETag は引用符や `W/` も含めて保存する。If-Modified-Since には配信元の Last-Modified を送る)。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Validators {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
}

impl Validators {
    /// 応答のヘッダから読む。
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let value = |name| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        };
        Self {
            etag: value(ETAG),
            last_modified: value(LAST_MODIFIED),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.etag.is_none() && self.last_modified.is_none()
    }

    /// 304 の応答で更新したもの。応答に書かれた検証子を使い、書かれていなければ今のまま
    /// (304 は、保存している応答のヘッダを更新する)。
    pub fn updated_by(&self, headers: &HeaderMap) -> Self {
        let fresh = Self::from_headers(headers);
        Self {
            etag: fresh.etag.or_else(|| self.etag.clone()),
            last_modified: fresh.last_modified.or_else(|| self.last_modified.clone()),
        }
    }

    /// 条件付きの取得のヘッダを付ける。
    fn apply(&self, mut request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(etag) = &self.etag {
            request = request.header(IF_NONE_MATCH, etag);
        }
        if let Some(last_modified) = &self.last_modified {
            request = request.header(IF_MODIFIED_SINCE, last_modified);
        }
        request
    }
}

/// 画像を書いたときの長さと SHA-256 (16 進)。読むときに照らし、書いたあとで壊れた画像 (電源が落ちて
/// 書ききれなかった・ディスクの不良など) を見つける。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Integrity {
    pub len: u64,
    pub sha256: String,
}

impl Integrity {
    pub fn of(bytes: &[u8]) -> Self {
        Self {
            len: bytes.len() as u64,
            sha256: sha256_hex(bytes),
        }
    }

    /// このアプリが書いた形か (SHA-256 が 64 桁の小文字の 16 進)。形の違うものは照らすのに使わない
    /// (`.meta` のほうが壊れていて、画像が壊れたとは言えない)。
    fn is_well_formed(&self) -> bool {
        self.sha256.len() == 64
            && self
                .sha256
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// `.meta` の中身。前の版の `.meta` (検証子だけ) もそのまま読む。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CacheMeta {
    #[serde(flatten)]
    validators: Validators,
    /// 書いたときの長さと SHA-256。これを入れる前の版で書いた画像にはない。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    integrity: Option<Integrity>,
}

/// キャッシュから読んだ画像と、読んだときの世代。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheRead {
    /// 読んだ画像の世代 ([`ImageCache::generation`])。あとで入れ直されていないかを照らすのに使う。
    pub generation: u64,
    pub state: CacheState,
}

/// キャッシュの画像を、書いたときの長さと SHA-256 で照らした結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheState {
    /// 書いたときのまま。`sha256` は画像の SHA-256 (16 進)。
    Intact {
        bytes: Vec<u8>,
        sha256: String,
        validators: Validators,
    },
    /// 照らすものがない (これを入れる前の版で書いた画像・`.meta` がない・読めない)。壊れていないとは言えない。
    Unverified { bytes: Vec<u8>, sha256: String },
    /// 書いたときの長さか SHA-256 に合わない (書いたあとで壊れた)。
    Corrupted,
    /// キャッシュにない。
    Missing,
}

impl CacheState {
    /// 読めた画像のバイト列 (`Intact`・`Unverified`)。
    pub fn bytes(&self) -> Option<&[u8]> {
        match self {
            CacheState::Intact { bytes, .. } | CacheState::Unverified { bytes, .. } => Some(bytes),
            CacheState::Corrupted | CacheState::Missing => None,
        }
    }
}

/// [`ImageCache::discard_unverified`] の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Discard {
    /// 消した。
    Removed,
    /// もうなかった (prune など)。
    Missing,
    /// 読んだあとで入れ直されていた。消さない。
    Replaced,
    /// 同じバイト列が、配信元と同じと確かめられていた。消さない (取り直しても同じバイト列になる)。
    Verified,
}

/// 画像を取りに行って、キャッシュに入れた結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refreshed {
    /// キャッシュになかった画像を入れた。
    Added,
    /// 配信元で中身が変わっていたので入れ直した。
    Changed,
    /// 変わっていなかった (304、または同じバイト列)。確かめた時刻と検証子だけを進めた。取りに行っている
    /// あいだにほかが入れ直していたときも、これにする (遅れて届いた結果では上書きしない)。
    Unchanged,
}

/// 画像を取れなかった理由。
#[derive(Debug, thiserror::Error)]
pub enum ImageFetchError {
    /// 配信元が 2xx・304 以外を返した。
    #[error("画像を取れません: HTTP {status}")]
    Status {
        status: StatusCode,
        /// 429・503 の Retry-After (秒で書かれたもの)。
        retry_after: Option<Duration>,
    },
    /// 2xx だが画像ではない (WAF の確かめのページなど)。キャッシュには入れない。
    #[error("画像ではない応答です (Content-Type: {content_type})")]
    NotImage { content_type: String },
    /// 接続・応答の読み込みの失敗 (タイムアウト・切断・大きすぎる応答など)。
    #[error("画像を取れません: {0:#}")]
    Transport(anyhow::Error),
    /// キャッシュに書けない。
    #[error("画像をキャッシュに入れられません: {0:#}")]
    Cache(anyhow::Error),
    /// 取りに行かなかった (手元のネットワークの URL・解釈できない URL・取得 URL の記録がないなど)。
    #[error("{0}")]
    Refused(String),
}

impl ImageFetchError {
    /// 時間をおけば取れるかもしれない (通信の失敗・5xx・429)。
    pub fn is_transient(&self) -> bool {
        match self {
            ImageFetchError::Status { status, .. } => {
                status.is_server_error() || *status == StatusCode::TOO_MANY_REQUESTS
            }
            ImageFetchError::Transport(_) => true,
            ImageFetchError::NotImage { .. }
            | ImageFetchError::Cache(_)
            | ImageFetchError::Refused(_) => false,
        }
    }

    /// 署名の期限が切れた URL で返ることのある応答 (401・403・404・410)。期限切れとは限らない
    /// (403 は配信元の WAF のこともある)。
    pub fn may_be_expired(&self) -> bool {
        matches!(
            self,
            ImageFetchError::Status { status, .. } if matches!(status.as_u16(), 401 | 403 | 404 | 410)
        )
    }

    /// 配信元が指定した、次に試してよいまでの時間。
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            ImageFetchError::Status { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

/// キャッシュに入っている画像 1 枚。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheEntry {
    /// 最後に配信元で確かめた時刻 (画像のファイルの更新時刻)。
    pub checked_at: SystemTime,
    pub size: u64,
}

/// 確かめた時刻を進めようとした結果。
enum Confirmed {
    Done,
    /// 取りに行っているあいだに、ほかが入れ直していた。
    Superseded,
    /// 画像が消えていた (prune など)。
    Missing,
}

impl ImageCache {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            generations: Arc::default(),
            #[cfg(test)]
            read_pause: Arc::default(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 鍵にする URL。フラグメントだけを外す (サーバーへは送られないため)。
    pub fn normalize(url: &str) -> String {
        match Url::parse(url) {
            Ok(mut parsed) => {
                parsed.set_fragment(None);
                parsed.into()
            }
            Err(_) => url.to_string(),
        }
    }

    /// アプリの中だけで使う画像の URL (`.invalid` のホスト) か。画像の参照
    /// (`https://source-image.invalid/...`) と旧形式の画像 (`https://legacy-image.invalid/...`) で、
    /// キャッシュにあるものだけを出し、そのまま外へは取りに行かない。
    pub fn is_internal(url: &str) -> bool {
        Url::parse(url)
            .ok()
            .and_then(|parsed| parsed.host_str().map(|host| host.ends_with(".invalid")))
            .unwrap_or(false)
    }

    pub fn path_for(&self, url: &str) -> PathBuf {
        let digest = Sha256::digest(Self::normalize(url).as_bytes());
        self.dir.join(hex::encode(&digest[..16]))
    }

    fn meta_path_for(&self, url: &str) -> PathBuf {
        self.path_for(url).with_extension(META_EXTENSION)
    }

    /// キャッシュにあるか。
    pub fn contains(&self, url: &str) -> bool {
        self.path_for(url).is_file()
    }

    /// 入っている画像のバイト列。書いたときの長さと SHA-256 では照らさない (照らすのは
    /// [`ImageCache::read_checked`])。
    pub fn read(&self, url: &str) -> Option<Vec<u8>> {
        fs::read(self.path_for(url)).ok()
    }

    /// 入っている画像を読み、書いたときの長さと SHA-256 で照らす。
    ///
    /// ふだんはロックを取らずに読む (画面の読み込みを 1 つのロックで並ばせない)。読む前後で世代が変わった
    /// (画像と `.meta` が別の書き込みのものかもしれない) か、照らして合わなかったときは、世代のロックの中で
    /// 読み直して照らす (このプロセスの書き込みはロックの中で行うので、食い違わない)。ほかのプロセス
    /// (`--fetch-once`) の書き込みとは食い違うことがあるが、そのときは捨てて取り直すだけになる。
    pub fn read_checked(&self, url: &str) -> CacheRead {
        let path = self.path_for(url);
        let meta = self.meta_path_for(url);
        let before = self.generation_at(&path);
        let state = read_state(&path, &meta, || self.pause_read());
        if !matches!(state, CacheState::Corrupted) && self.generation_at(&path) == before {
            return CacheRead {
                generation: before,
                state,
            };
        }
        let generations = self.generations.lock();
        CacheRead {
            generation: generations.get(&path).copied().unwrap_or(0),
            state: read_state(&path, &meta, || {}),
        }
    }

    /// テストで、ロックを取らずに読んでいる途中 (`.meta` を読んだあと画像を読む前) に一度だけ止める。
    fn pause_read(&self) {
        #[cfg(test)]
        if let Some(pause) = self.read_pause.0.lock().take() {
            pause();
        }
    }

    /// 書いたときの長さか SHA-256 に合わない画像を、`.meta` と一緒に消す。ロックの中で照らし直し、合わな
    /// ければ消す (消したら `true`)。照らすものがない画像・合う画像は消さない。
    pub fn discard_corrupted(&self, url: &str) -> Result<bool> {
        let mut generations = self.generations.lock();
        let path = self.path_for(url);
        let meta = self.meta_path_for(url);
        if !matches!(read_state(&path, &meta, || {}), CacheState::Corrupted) {
            return Ok(false);
        }
        self.remove_locked(&mut generations, &path, &meta)?;
        Ok(true)
    }

    /// `generation` の世代で確かめていない画像 ([`CacheState::Unverified`]) として読んだ、SHA-256 が `sha256` の
    /// 画像を、まだそのまま入っていれば `.meta` と一緒に消す (画面が gpui で読めなかった画像を取り直すため)。
    ///
    /// 読んだあとで入れ直された画像 (世代か SHA-256 が違う) と、同じバイト列が配信元と同じと確かめられた画像
    /// (先読みが同じバイト列を取って `.meta` を書いた) は消さない。照らして合わない画像は消す。このプロセスの
    /// 書き込みとは世代のロックで並ぶが、ほかのプロセス (`--fetch-once`) が同時に入れ直した画像は、まれに消して
    /// しまうことがある (そのときは取り直すだけ)。
    pub fn discard_unverified(&self, url: &str, generation: u64, sha256: &str) -> Result<Discard> {
        let mut generations = self.generations.lock();
        let path = self.path_for(url);
        let meta = self.meta_path_for(url);
        let current = generations.get(&path).copied().unwrap_or(0);
        let discard = match read_state(&path, &meta, || {}) {
            CacheState::Missing => return Ok(Discard::Missing),
            _ if current != generation => return Ok(Discard::Replaced),
            CacheState::Intact { sha256: now, .. } if now == sha256 => {
                return Ok(Discard::Verified);
            }
            CacheState::Intact { .. } => return Ok(Discard::Replaced),
            CacheState::Unverified { sha256: now, .. } if now != sha256 => {
                return Ok(Discard::Replaced);
            }
            CacheState::Unverified { .. } | CacheState::Corrupted => Discard::Removed,
        };
        self.remove_locked(&mut generations, &path, &meta)?;
        Ok(discard)
    }

    /// 画像と `.meta` を消し、世代を進める (取りに行く前の世代で、遅れて届いた 304 を確認にしない)。画像を
    /// 先に消す。画像を消せなければ何も変えずに誤りにする。画像を消せたあとで `.meta` を消せなくても (ほかの
    /// プログラムが開いているなど) 消せたことにする (画像のない `.meta` は読まれず、prune が消す)。
    fn remove_locked(
        &self,
        generations: &mut HashMap<PathBuf, u64>,
        path: &Path,
        meta: &Path,
    ) -> Result<()> {
        remove_if_exists(path)?;
        *generations.entry(path.to_path_buf()).or_insert(0) += 1;
        if let Err(err) = remove_if_exists(meta) {
            tracing::warn!("画像を消したあとで、横の .meta を消せません: {err:#}");
        }
        Ok(())
    }

    /// 入っている画像の世代。取りに行く前に読み、入れるときに照らす。
    pub fn generation(&self, url: &str) -> u64 {
        self.generation_at(&self.path_for(url))
    }

    fn generation_at(&self, path: &Path) -> u64 {
        self.generations.lock().get(path).copied().unwrap_or(0)
    }

    /// 検証子なしで入れる (配信元で確かめ直せない画像。旧形式から取り込んだ手元の画像など)。
    pub fn write(&self, url: &str, bytes: &[u8]) -> Result<()> {
        self.store(url, bytes, &Validators::default())
    }

    /// 配信元から取ってきた画像を、そのときの検証子と一緒に入れる。
    pub fn store(&self, url: &str, bytes: &[u8], validators: &Validators) -> Result<()> {
        let mut generations = self.generations.lock();
        self.store_locked(&mut generations, url, bytes, validators)
    }

    /// [`ImageCache::store`] と同じだが、`generation` (取りに行く前の世代) のあとでほかが入れ直していたら
    /// 入れない (ほかが入れた画像を残す)。ただし、その画像も消えていれば (prune など) 入れる。入れたら `true`。
    pub fn store_if(
        &self,
        url: &str,
        generation: u64,
        bytes: &[u8],
        validators: &Validators,
    ) -> Result<bool> {
        let mut generations = self.generations.lock();
        let path = self.path_for(url);
        if generations.get(&path).copied().unwrap_or(0) != generation && path.is_file() {
            return Ok(false);
        }
        self.store_locked(&mut generations, url, bytes, validators)?;
        Ok(true)
    }

    /// 先に前の `.meta` を消してから画像を書き、最後に新しい `.meta` (検証子と、書いた長さと SHA-256) を書く。
    /// 途中で止まっても、前の画像の検証子が新しい画像に残る (変わっていない扱いで古い画像を出し続ける) ことは
    /// ない (`.meta` のない画像は、確かめていない画像として扱う)。前の `.meta` を消せなければ、画像も書き換えない。
    fn store_locked(
        &self,
        generations: &mut HashMap<PathBuf, u64>,
        url: &str,
        bytes: &[u8],
        validators: &Validators,
    ) -> Result<()> {
        fs::create_dir_all(&self.dir)
            .with_context(|| format!("{} を作成できません", self.dir.display()))?;
        let path = self.path_for(url);
        let meta = self.meta_path_for(url);
        remove_if_exists(&meta)?;
        self.replace(&path, bytes)?;
        *generations.entry(path).or_insert(0) += 1;
        self.write_meta(&meta, validators, Integrity::of(bytes))
    }

    fn write_meta(&self, meta: &Path, validators: &Validators, integrity: Integrity) -> Result<()> {
        let meta_value = CacheMeta {
            validators: validators.clone(),
            integrity: Some(integrity),
        };
        self.replace(meta, &serde_json::to_vec(&meta_value)?)
    }

    /// 入っている画像の検証子。なければ (前の版のキャッシュ・配信元が返さなかった・壊れている) 空。
    pub fn validators(&self, url: &str) -> Validators {
        read_meta(&self.meta_path_for(url))
            .map(|meta| meta.validators)
            .unwrap_or_default()
    }

    /// 入っている画像を最後に配信元で確かめた時刻 (画像のファイルの更新時刻)。なければ `None`。
    pub fn checked_at(&self, url: &str) -> Option<SystemTime> {
        self.entry(url).map(|entry| entry.checked_at)
    }

    /// 入っている画像の、確かめた時刻と大きさ。なければ `None`。
    pub fn entry(&self, url: &str) -> Option<CacheEntry> {
        let metadata = fs::metadata(self.path_for(url)).ok()?;
        metadata.is_file().then_some(())?;
        Some(CacheEntry {
            checked_at: metadata.modified().ok()?,
            size: metadata.len(),
        })
    }

    /// 入っている画像が配信元で変わっていないと分かった。`.meta` を検証子と `integrity` (照らして合った・配信元の
    /// バイト列と同じと分かった画像の長さと SHA-256) で書き直し、確かめた時刻を今にする。画像が消えていれば
    /// (prune など。ほかが入れ直したあとでも) `Missing`、`generation` のあとでほかが入れ直していたら何もしない。
    fn confirm_if(
        &self,
        url: &str,
        generation: u64,
        validators: &Validators,
        integrity: Integrity,
    ) -> Result<Confirmed> {
        let generations = self.generations.lock();
        let path = self.path_for(url);
        if !path.is_file() {
            return Ok(Confirmed::Missing);
        }
        if generations.get(&path).copied().unwrap_or(0) != generation {
            return Ok(Confirmed::Superseded);
        }
        self.write_meta(&self.meta_path_for(url), validators, integrity)?;
        touch(&path)?;
        Ok(Confirmed::Done)
    }

    /// 書きかけのファイルに書いて閉じてから置き換える。書きかけの名前は書き込みごとに変える (同じ画像を
    /// 先読みと画面が同時に書いても、1 つの書きかけを分け合わない)。書くのにも置き換えるのにも失敗したら、
    /// 書きかけを消す。
    ///
    /// Windows は、置き換える先をほかのプログラム (ウイルス対策の検査など) が開いていると、置き換えを断る
    /// ことがある (`ERROR_ACCESS_DENIED`。書いた直後の画像をすぐ置き換えるときなど)。そのときは前の画像が
    /// 残るだけなので、待ってやり直すことはしない (呼び出し元は async の中で同期のまま呼ぶ)。
    fn replace(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let tmp = self.dir.join(format!(
            "{name}.{}.{}.{TEMP_EXTENSION}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let result = fs::write(&tmp, bytes)
            .with_context(|| format!("{} を書けません", tmp.display()))
            .and_then(|()| {
                fs::rename(&tmp, path)
                    .with_context(|| format!("{} を保存できません", path.display()))
            });
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }

    /// 古いものから消し、合計を `max_bytes` 以下にする。`older_than` より古いものも消す。消した画像の数と
    /// 大きさを返す。画像とその検証子は組で数えて一緒に消し、画像のない検証子と、書きかけのまま残った
    /// ファイルも消す。消す直前に時刻を見直し、数えたあとで確かめ直された画像は消さない。
    pub fn prune(&self, older_than: SystemTime, max_bytes: u64) -> Result<(usize, u64)> {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Ok((0, 0));
        };
        let now = SystemTime::now();
        let mut files: Vec<CachedFile> = Vec::new();
        let mut metas: Vec<(PathBuf, u64)> = Vec::new();
        let mut freed = 0;
        for entry in entries.filter_map(|entry| entry.ok()) {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            let path = entry.path();
            let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            match path.extension().and_then(|extension| extension.to_str()) {
                Some(META_EXTENSION) => metas.push((path, metadata.len())),
                Some(TEMP_EXTENSION) => {
                    let stale = now.duration_since(modified).unwrap_or_default() > TEMP_KEEP;
                    if stale && fs::remove_file(&path).is_ok() {
                        freed += metadata.len();
                    }
                }
                _ => files.push(CachedFile {
                    modified,
                    size: metadata.len(),
                    path,
                    meta: None,
                }),
            }
        }
        for (meta, size) in metas {
            let image = meta.with_extension("");
            match files.iter_mut().find(|file| file.path == image) {
                Some(file) => {
                    file.size += size;
                    file.meta = Some(meta);
                }
                None => {
                    // 数えたあとで画像と一緒に入った検証子は消さない。
                    let _generations = self.generations.lock();
                    if !image.is_file() && fs::remove_file(&meta).is_ok() {
                        freed += size;
                    }
                }
            }
        }
        files.sort_by_key(|file| file.modified);

        let mut total: u64 = files.iter().map(|file| file.size).sum();
        let mut removed = 0;
        for file in files {
            if file.modified >= older_than && total <= max_bytes {
                break;
            }
            let _generations = self.generations.lock();
            let current = fs::metadata(&file.path)
                .ok()
                .and_then(|metadata| metadata.modified().ok());
            if current != Some(file.modified) {
                // 数えたあとで確かめ直された・入れ直された (消えた) 画像。
                continue;
            }
            if fs::remove_file(&file.path).is_ok() {
                if let Some(meta) = &file.meta {
                    let _ = fs::remove_file(meta);
                }
                removed += 1;
                freed += file.size;
                total -= file.size;
            }
        }
        Ok((removed, freed))
    }
}

struct CachedFile {
    path: PathBuf,
    modified: SystemTime,
    size: u64,
    meta: Option<PathBuf>,
}

/// `.meta` を読む。ない・読めない・形が違えば `None`。
fn read_meta(meta: &Path) -> Option<CacheMeta> {
    fs::read(meta)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

/// 画像と `.meta` を読んで、書いたときの長さと SHA-256 で照らす。`.meta` がない・読めない・形が違うときは
/// 照らすものがない (`Unverified`) とし、壊れたとはしない。`between` は `.meta` を読んだあと画像を読む前に
/// 呼ぶ (テストの止め場所)。
fn read_state(path: &Path, meta: &Path, between: impl FnOnce()) -> CacheState {
    let meta = read_meta(meta);
    between();
    let Ok(bytes) = fs::read(path) else {
        return CacheState::Missing;
    };
    let sha256 = sha256_hex(&bytes);
    match meta {
        Some(CacheMeta {
            validators,
            integrity: Some(integrity),
        }) if integrity.is_well_formed() => {
            if integrity.len == bytes.len() as u64 && integrity.sha256 == sha256 {
                CacheState::Intact {
                    bytes,
                    sha256,
                    validators,
                }
            } else {
                CacheState::Corrupted
            }
        }
        _ => CacheState::Unverified { bytes, sha256 },
    }
}

fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("{} を消せません", path.display())),
    }
}

/// ファイルの更新時刻を今にする。
fn touch(path: &Path) -> Result<()> {
    fs::File::options()
        .write(true)
        .open(path)
        .and_then(|file| file.set_modified(SystemTime::now()))
        .with_context(|| format!("{} の時刻を更新できません", path.display()))
}

/// 画像として入れてよい応答か。Content-Type が `image/` なら画像とし、それ以外 (なし・
/// `application/octet-stream` など) は中身の先頭で見分ける。HTML (WAF の確かめのページなど) は入れない。
pub fn looks_like_image(content_type: Option<&str>, bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(512)];
    let text = String::from_utf8_lossy(head);
    let text = text.trim_start_matches('\u{feff}').trim_start();
    let lower = text.to_ascii_lowercase();
    if lower.starts_with("<!doctype html") || lower.starts_with("<html") {
        return false;
    }
    let mime = content_type
        .and_then(|value| value.split(';').next())
        .map(|value| value.trim().to_ascii_lowercase());
    if mime
        .as_deref()
        .is_some_and(|mime| mime.starts_with("image/"))
    {
        return true;
    }
    let magic: &[&[u8]] = &[
        b"\x89PNG\r\n\x1a\n",
        b"\xff\xd8\xff",
        b"GIF87a",
        b"GIF89a",
        b"BM",
        b"\x00\x00\x01\x00",
        b"II*\x00",
        b"MM\x00*",
    ];
    magic.iter().any(|magic| bytes.starts_with(magic))
        || (bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP")
        || (bytes.len() >= 12 && &bytes[4..8] == b"ftyp")
        || lower.starts_with("<svg")
        || (lower.starts_with("<?xml") && lower.contains("<svg"))
}

/// `url` の画像を取り、キャッシュの `key` (画像の参照か、`url` そのもの) に入れる。
///
/// キャッシュにあり、書いたときの長さと SHA-256 に合えば、入れたときの検証子で条件付きに取る。確かめていない
/// 画像 (これを入れる前の版で書いたもの)・壊れた画像は、304 で延命しないよう条件なしで取る。304 か同じバイト列
/// なら、確かめた時刻と `.meta` だけを進める (画像は書き直さない。確かめていない画像は、ここで配信元と同じと
/// 分かったことになる)。304 のときに画像が消えていたら、条件なしで取り直す。2xx でも 304 でもない応答・画像では
/// ない応答は誤りにし、キャッシュは変えない (使えていた画像を残す)。取りに行っているあいだにほかが入れ直して
/// いたら、その画像を残す。取得先の制限 (手元のネットワークへ取りに行かない) は、呼び出し側がクライアントと
/// URL で確かめる。
pub async fn fetch_image(
    client: &reqwest::Client,
    cache: &ImageCache,
    url: &str,
    key: &str,
    max_bytes: usize,
) -> Result<Refreshed, ImageFetchError> {
    let mut conditional = true;
    loop {
        let CacheRead { generation, state } = cache.read_checked(key);
        let cached = !matches!(state, CacheState::Missing);
        // 条件付きに取るのは、照らして合った画像だけ (そのときの長さと SHA-256 を、304 のあとも残す)。
        let condition = match &state {
            CacheState::Intact {
                bytes,
                sha256,
                validators,
            } if conditional && !validators.is_empty() => Some((
                validators.clone(),
                Integrity {
                    len: bytes.len() as u64,
                    sha256: sha256.clone(),
                },
            )),
            _ => None,
        };
        let request = match &condition {
            Some((validators, _)) => validators.apply(client.get(url)),
            None => client.get(url),
        };
        let response = request
            .send()
            .await
            .map_err(|err| ImageFetchError::Transport(err.into()))?;
        let status = response.status();
        if status == StatusCode::NOT_MODIFIED
            && let Some((validators, integrity)) = condition
        {
            match cache
                .confirm_if(
                    key,
                    generation,
                    &validators.updated_by(response.headers()),
                    integrity,
                )
                .map_err(ImageFetchError::Cache)?
            {
                Confirmed::Done | Confirmed::Superseded => return Ok(Refreshed::Unchanged),
                Confirmed::Missing => {
                    conditional = false;
                    continue;
                }
            }
        }
        if !status.is_success() {
            return Err(ImageFetchError::Status {
                status,
                retry_after: retry_after(response.headers()),
            });
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let fresh = Validators::from_headers(response.headers());
        let bytes = crate::http::read_capped(response, max_bytes)
            .await
            .map_err(ImageFetchError::Transport)?;
        if !looks_like_image(content_type.as_deref(), &bytes) {
            return Err(ImageFetchError::NotImage {
                content_type: content_type.unwrap_or_else(|| "なし".into()),
            });
        }
        if state.bytes().is_some_and(|old| old == &bytes[..]) {
            match cache
                .confirm_if(key, generation, &fresh, Integrity::of(&bytes))
                .map_err(ImageFetchError::Cache)?
            {
                Confirmed::Done | Confirmed::Superseded => return Ok(Refreshed::Unchanged),
                Confirmed::Missing => {}
            }
        }
        let stored = cache
            .store_if(key, generation, &bytes, &fresh)
            .map_err(ImageFetchError::Cache)?;
        return Ok(match (stored, cached) {
            (false, _) => Refreshed::Unchanged,
            (true, true) => Refreshed::Changed,
            (true, false) => Refreshed::Added,
        });
    }
}

/// 秒で書かれた Retry-After。
fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    #[test]
    fn keys_keep_queries_and_drop_fragments() {
        // 問い合わせは推測で外さない。版 (`v`) が変われば別の画像として取り直す。
        assert_eq!(
            ImageCache::normalize("https://cdn.example.com/a.png?t=123&id=7#x"),
            "https://cdn.example.com/a.png?t=123&id=7"
        );
        assert_ne!(
            ImageCache::normalize("https://cdn.example.com/a.png?v=2"),
            ImageCache::normalize("https://cdn.example.com/a.png?v=3")
        );
        assert_eq!(ImageCache::normalize("not a url"), "not a url");
    }

    #[test]
    fn recognizes_internal_images() {
        assert!(ImageCache::is_internal(
            "https://source-image.invalid/gc/0123456789abcdef0123456789abcdef"
        ));
        assert!(ImageCache::is_internal(
            "https://legacy-image.invalid/0123456789abcdef"
        ));
        assert!(!ImageCache::is_internal("https://cdn.example.com/a.png"));
        assert!(!ImageCache::is_internal("not a url"));
    }

    fn set_modified(cache: &ImageCache, url: &str, at: SystemTime) {
        fs::File::options()
            .write(true)
            .open(cache.path_for(url))
            .unwrap()
            .set_modified(at)
            .unwrap();
    }

    fn validators(etag: &str) -> Validators {
        Validators {
            etag: Some(etag.into()),
            last_modified: Some("Wed, 04 Nov 2026 01:00:00 GMT".into()),
        }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn writes_reads_and_prunes() {
        let dir = tempfile::tempdir().unwrap();
        let images = dir.path().join("images");
        let cache = ImageCache::new(images.clone());
        cache
            .store(
                "https://example.com/a.png#top",
                b"aaaa",
                &validators("\"a\""),
            )
            .unwrap();
        assert_eq!(
            cache.read("https://example.com/a.png").as_deref(),
            Some(&b"aaaa"[..])
        );
        assert_eq!(
            cache.validators("https://example.com/a.png"),
            validators("\"a\"")
        );
        cache.write("https://example.com/b.png", b"bb").unwrap();
        // 検証子がなくても、書いた長さと SHA-256 を `.meta` に残す。
        assert!(cache.validators("https://example.com/b.png").is_empty());
        let b_meta = cache.meta_path_for("https://example.com/b.png");
        assert!(b_meta.is_file());
        // 画像のない検証子 (画像だけ手で消したなど) と、書きかけのまま残った古いファイルは消す。
        let orphan = images.join(format!("{}.meta", "0".repeat(32)));
        fs::write(&orphan, b"{}").unwrap();
        let left_over = images.join(format!("{}.1.2.tmp", "1".repeat(32)));
        fs::write(&left_over, b"xyz").unwrap();
        let fresh_tmp = images.join(format!("{}.1.3.tmp", "2".repeat(32)));
        fs::write(&fresh_tmp, b"w").unwrap();
        let old = SystemTime::now() - Duration::from_secs(2 * 3600);
        fs::File::options()
            .write(true)
            .open(&left_over)
            .unwrap()
            .set_modified(old)
            .unwrap();
        // 同じ時刻だとどちらが古いか決まらないので、a.png を古くしておく。
        set_modified(&cache, "https://example.com/a.png", old);

        // 上限を超えた分だけ、古いものから消す。画像と検証子は組で数え、一緒に消す (上限は b.png の組の大きさ)。
        let meta_size = fs::metadata(cache.meta_path_for("https://example.com/a.png"))
            .unwrap()
            .len();
        let b_size = 2 + fs::metadata(&b_meta).unwrap().len();
        let (removed, freed) = cache
            .prune(SystemTime::now() - Duration::from_secs(3 * 3600), b_size)
            .unwrap();
        assert_eq!((removed, freed), (1, 4 + meta_size + 2 + 3));
        assert!(cache.read("https://example.com/a.png").is_none());
        assert!(!cache.meta_path_for("https://example.com/a.png").exists());
        assert!(cache.read("https://example.com/b.png").is_some());
        // 書いている途中かもしれない新しい書きかけのファイルは残す。
        let b = file_name(&cache, "https://example.com/b.png");
        let mut expected = vec![
            format!("{b}.meta"),
            b,
            fresh_tmp
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        ];
        expected.sort();
        assert_eq!(names(&images), expected);
    }

    #[test]
    fn storing_new_bytes_drops_the_old_validators_and_late_results_do_not_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ImageCache::new(dir.path().join("images"));
        let url = "https://source-image.invalid/gc/0123456789abcdef0123456789abcdef";
        assert_eq!(cache.checked_at(url), None);
        assert!(matches!(
            cache
                .confirm_if(url, 0, &validators("\"a\""), Integrity::of(b"old"))
                .unwrap(),
            Confirmed::Missing
        ));
        let before = cache.generation(url);
        cache.store(url, b"old", &validators("\"a\"")).unwrap();
        // 検証子を返さない配信元の画像に入れ替えたら、前の画像の検証子は残さない。
        cache.store(url, b"new", &Validators::default()).unwrap();
        assert!(cache.validators(url).is_empty());
        assert_eq!(cache.read(url).as_deref(), Some(&b"new"[..]));

        // 取りに行く前の世代のあとで入れ直されていたら、遅れて届いた結果では上書きも確認もしない。
        assert!(
            !cache
                .store_if(url, before, b"late", &validators("\"z\""))
                .unwrap()
        );
        assert!(matches!(
            cache
                .confirm_if(url, before, &validators("\"z\""), Integrity::of(b"old"))
                .unwrap(),
            Confirmed::Superseded
        ));
        assert_eq!(cache.read(url).as_deref(), Some(&b"new"[..]));
        // 入れ直された画像もそのあとで消えていたら (prune など)、確認にはせず、遅れて届いた画像を入れる。
        fs::remove_file(cache.path_for(url)).unwrap();
        assert!(matches!(
            cache
                .confirm_if(url, before, &validators("\"z\""), Integrity::of(b"old"))
                .unwrap(),
            Confirmed::Missing
        ));
        assert!(
            cache
                .store_if(url, before, b"late", &Validators::default())
                .unwrap()
        );
        assert_eq!(cache.read(url).as_deref(), Some(&b"late"[..]));
        cache.store(url, b"new", &Validators::default()).unwrap();

        // 変わっていないと分かったら、画像はそのままで、確かめた時刻を今にする。
        let old = SystemTime::now() - Duration::from_secs(3 * 24 * 3600);
        set_modified(&cache, url, old);
        let now = cache.generation(url);
        assert!(matches!(
            cache
                .confirm_if(url, now, &validators("\"b\""), Integrity::of(b"new"))
                .unwrap(),
            Confirmed::Done
        ));
        assert_eq!(cache.read(url).as_deref(), Some(&b"new"[..]));
        assert_eq!(cache.validators(url), validators("\"b\""));
        assert!(matches!(
            cache.read_checked(url).state,
            CacheState::Intact { .. }
        ));
        assert!(cache.checked_at(url).unwrap() > old + Duration::from_secs(24 * 3600));
        // 書きかけのファイルは残らない。
        assert!(
            names(&dir.path().join("images"))
                .iter()
                .all(|name| !name.ends_with(".tmp"))
        );
    }

    #[test]
    fn rejects_pages_that_are_not_images() {
        assert!(looks_like_image(
            Some("image/png"),
            b"\x89PNG\r\n\x1a\n...."
        ));
        assert!(looks_like_image(
            Some("image/webp; charset=binary"),
            b"RIFF"
        ));
        // Content-Type がない・汎用のものは、中身の先頭で見分ける。
        assert!(looks_like_image(None, b"\xff\xd8\xff\xe0JFIF"));
        assert!(looks_like_image(
            Some("application/octet-stream"),
            b"RIFF\0\0\0\0WEBPVP8 "
        ));
        assert!(looks_like_image(Some("text/plain"), b"GIF89a...."));
        assert!(looks_like_image(
            Some("text/xml"),
            b"<?xml version=\"1.0\"?><svg></svg>"
        ));
        // WAF の確かめのページや JSON は入れない。
        assert!(!looks_like_image(
            Some("text/html"),
            b"<!DOCTYPE html><html></html>"
        ));
        assert!(!looks_like_image(
            Some("image/png"),
            b"\n  <html><body>blocked</body></html>"
        ));
        assert!(!looks_like_image(
            Some("application/json"),
            b"{\"error\":\"expired\"}"
        ));
        assert!(!looks_like_image(None, b"Access Denied"));
    }

    /// 決まった応答を順に返す HTTP サーバー。受けたリクエストの頭 (ヘッダまで) を小文字で残す。
    async fn serve(responses: Vec<String>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let log = requests.clone();
        tokio::spawn(async move {
            for response in responses {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let mut buffer = vec![0u8; 8192];
                let read = stream.read(&mut buffer).await.unwrap_or(0);
                log.lock()
                    .push(String::from_utf8_lossy(&buffer[..read]).to_lowercase());
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        (format!("http://{address}/a.png"), requests)
    }

    fn response(status: &str, headers: &[&str], body: &str) -> String {
        let mut text = format!("HTTP/1.1 {status}\r\nConnection: close\r\n");
        for header in headers {
            text.push_str(header);
            text.push_str("\r\n");
        }
        text.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
        text
    }

    const PNG: &str = "image/png";

    #[tokio::test]
    async fn revalidates_cached_images_with_their_validators() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ImageCache::new(dir.path().join("images"));
        let key = "https://source-image.invalid/gc/0123456789abcdef0123456789abcdef";
        let png = format!("Content-Type: {PNG}");
        let png = png.as_str();
        let (url, requests) = serve(vec![
            // 1: キャッシュにないので、条件なしで取る。
            response(
                "200 OK",
                &[
                    png,
                    "ETag: \"v1\"",
                    "Last-Modified: Wed, 04 Nov 2026 01:00:00 GMT",
                ],
                "GIF89a-1",
            ),
            // 2: 変わっていない。
            response("304 Not Modified", &["ETag: \"v1\""], ""),
            // 3: 差し替わった。
            response("200 OK", &[png, "ETag: \"v2\""], "GIF89a-2"),
            // 4: 検証子を見ずに同じものを返す配信元。
            response("200 OK", &[png, "ETag: \"v3\""], "GIF89a-2"),
            // 5: 期限切れなど。
            response("403 Forbidden", &[], "denied"),
            // 6: WAF の確かめのページ。
            response("200 OK", &["Content-Type: text/html"], "<html>wait</html>"),
            // 7: 混んでいる。
            response("429 Too Many Requests", &["Retry-After: 120"], ""),
        ])
        .await;
        let client = reqwest::Client::new();
        let fetch = || fetch_image(&client, &cache, &url, key, 1024);

        assert_eq!(fetch().await.unwrap(), Refreshed::Added);
        assert_eq!(cache.read(key).as_deref(), Some(&b"GIF89a-1"[..]));
        assert_eq!(
            cache.validators(key),
            Validators {
                etag: Some("\"v1\"".into()),
                last_modified: Some("Wed, 04 Nov 2026 01:00:00 GMT".into()),
            }
        );
        assert!(!requests.lock()[0].contains("if-none-match"));

        // 304: 画像はそのまま、確かめた時刻を進める。応答に Last-Modified がなければ前の値を使う。
        let old = SystemTime::now() - Duration::from_secs(2 * 24 * 3600);
        set_modified(&cache, key, old);
        assert_eq!(fetch().await.unwrap(), Refreshed::Unchanged);
        assert!(requests.lock()[1].contains("if-none-match: \"v1\""));
        assert!(requests.lock()[1].contains("if-modified-since: wed, 04 nov 2026 01:00:00 gmt"));
        assert_eq!(cache.read(key).as_deref(), Some(&b"GIF89a-1"[..]));
        assert!(cache.checked_at(key).unwrap() > old);
        assert_eq!(
            cache.validators(key).last_modified.as_deref(),
            Some("Wed, 04 Nov 2026 01:00:00 GMT")
        );

        // 200 で中身が変わっていれば入れ直し、検証子も替える (前の Last-Modified は残さない)。
        assert_eq!(fetch().await.unwrap(), Refreshed::Changed);
        assert_eq!(cache.read(key).as_deref(), Some(&b"GIF89a-2"[..]));
        assert_eq!(
            cache.validators(key),
            Validators {
                etag: Some("\"v2\"".into()),
                last_modified: None,
            }
        );

        // 200 でも同じバイト列なら、変わったことにしない。
        set_modified(&cache, key, old);
        assert_eq!(fetch().await.unwrap(), Refreshed::Unchanged);
        assert!(cache.checked_at(key).unwrap() > old);
        assert_eq!(cache.validators(key).etag.as_deref(), Some("\"v3\""));

        // 取れない応答・画像でない応答は誤りにし、キャッシュは変えない (確かめた時刻も進めない)。
        set_modified(&cache, key, old);
        let err = fetch().await.unwrap_err();
        assert!(err.may_be_expired() && !err.is_transient(), "{err:#}");
        let err = fetch().await.unwrap_err();
        assert!(matches!(err, ImageFetchError::NotImage { .. }), "{err:#}");
        assert!(!err.may_be_expired() && !err.is_transient());
        let err = fetch().await.unwrap_err();
        assert!(err.is_transient() && !err.may_be_expired(), "{err:#}");
        assert_eq!(err.retry_after(), Some(Duration::from_secs(120)));
        assert_eq!(cache.read(key).as_deref(), Some(&b"GIF89a-2"[..]));
        assert!(cache.checked_at(key).unwrap() < old + Duration::from_secs(60));
    }

    #[tokio::test]
    async fn refetches_without_conditions_when_the_image_vanished_before_a_304() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ImageCache::new(dir.path().join("images"));
        let key = "https://cdn.example.com/a.png";
        cache
            .store(key, b"GIF89a-old", &validators("\"v1\""))
            .unwrap();
        // 条件付きのリクエストを受けてから 304 を返すまでのあいだに、画像が消える (prune など)。
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/a.png", listener.local_addr().unwrap());
        let image = cache.path_for(key);
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            let replies = [
                response("304 Not Modified", &[], ""),
                response(
                    "200 OK",
                    &["Content-Type: image/gif", "ETag: \"v1\""],
                    "GIF89a-new",
                ),
            ];
            for (index, reply) in replies.iter().enumerate() {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buffer = vec![0u8; 8192];
                let read = stream.read(&mut buffer).await.unwrap();
                requests.push(String::from_utf8_lossy(&buffer[..read]).to_lowercase());
                if index == 0 {
                    fs::remove_file(&image).unwrap();
                }
                stream.write_all(reply.as_bytes()).await.unwrap();
            }
            requests
        });
        let client = reqwest::Client::new();
        // 304 は消えた画像の確認にはせず、条件なしで取り直して入れる。
        assert_eq!(
            fetch_image(&client, &cache, &url, key, 1024).await.unwrap(),
            Refreshed::Added
        );
        assert_eq!(cache.read(key).as_deref(), Some(&b"GIF89a-new"[..]));
        let requests = server.await.unwrap();
        assert!(requests[0].contains("if-none-match: \"v1\""));
        assert!(!requests[1].contains("if-none-match"));
    }

    /// `url` の画像のファイルの名前。
    fn file_name(cache: &ImageCache, url: &str) -> String {
        cache
            .path_for(url)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    /// キャッシュのフォルダにあるのが `url` の画像と、その画像の `.meta` (なくてもよい) だけ。
    fn assert_only_the_image(cache: &ImageCache, url: &str) {
        let image = file_name(cache, url);
        let names = names(cache.dir());
        assert!(
            names == [image.clone()] || names == [image.clone(), format!("{image}.meta")],
            "{names:?}"
        );
        assert_ne!(cache.read_checked(url).state, CacheState::Corrupted);
    }

    /// Windows が置き換えを断っただけの失敗か。Windows は、置き換える先をほかのプログラム (ウイルス対策の
    /// 検査など) が開いていると置き換えを断る (`ERROR_ACCESS_DENIED`・`ERROR_SHARING_VIOLATION`)。書いた直後の
    /// 画像をすぐ置き換えるときに起きやすい。キャッシュは壊れない。macOS の rename は、開かれているファイルも
    /// 置き換えるので断られない。
    fn replace_refused(err: &anyhow::Error, path: &Path) -> bool {
        const ERROR_ACCESS_DENIED: i32 = 5;
        const ERROR_SHARING_VIOLATION: i32 = 32;
        cfg!(windows)
            && err.to_string() == format!("{} を保存できません", path.display())
            && err
                .root_cause()
                .downcast_ref::<io::Error>()
                .and_then(io::Error::raw_os_error)
                .is_some_and(|code| matches!(code, ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION))
    }

    /// 終わったら読み手を止める。書き手の確かめが失敗しても、読み手を待ち続けない。
    struct StopOnDrop<'a>(&'a std::sync::atomic::AtomicBool);

    impl Drop for StopOnDrop<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    /// 同じ URL に同時に書くスレッドの数。
    const WRITERS: usize = 8;

    /// `round` 回目の書き手 `writer` の中身。書き込みごとに長さ (16〜128 KiB) と中身を変え、混ざったら
    /// 分かるようにする。先頭の 8 バイトが回と書き手。
    fn image(round: usize, writer: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(round as u32).to_le_bytes());
        bytes.extend_from_slice(&(writer as u32).to_le_bytes());
        bytes.resize(16 * 1024 * (writer + 1), (round * WRITERS + writer) as u8);
        bytes
    }

    /// どの書き込みの中身そのものか (回, 書き手)。どれとも違えば (書きかけ・混ざったもの) `None`。
    fn identify(bytes: &[u8]) -> Option<(usize, usize)> {
        let field = |at: usize| -> Option<usize> {
            let field: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
            Some(u32::from_le_bytes(field) as usize)
        };
        let (round, writer) = (field(0)?, field(4)?);
        (writer < WRITERS && bytes == image(round, writer)).then_some((round, writer))
    }

    /// `WRITERS` 個のスレッドが同じ URL に違う中身をそろって書くことを `rounds` 回くり返し、並行して
    /// 読み手が読む。読めた中身・残った中身はどれか 1 つの書き込みの中身そのもので、書きかけは残らない。
    fn write_concurrently(rounds: usize) {
        let dir = tempfile::tempdir().unwrap();
        let cache = ImageCache::new(dir.path().join("images"));
        let url = "https://example.com/a.png";
        let path = cache.path_for(url);
        // 前の画像。どの書き込みも断られた回は、前の中身が残る。
        cache.write(url, &image(0, 0)).unwrap();
        let mut kept = (0, 0);
        let mut succeeded = 0;
        let stop = std::sync::atomic::AtomicBool::new(false);
        let first_read = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            // 書いている間も読む。
            let reader = scope.spawn(|| {
                let mut reads = 0;
                while !stop.load(Ordering::Relaxed) {
                    if let Some(bytes) = cache.read(url) {
                        assert!(
                            identify(&bytes).is_some(),
                            "書きかけか、混ざった中身を読んだ ({} バイト)",
                            bytes.len()
                        );
                        reads += 1;
                        if reads == 1 {
                            first_read.wait();
                        }
                    }
                }
                reads
            });

            {
                let _stop = StopOnDrop(&stop);
                // 読み手が一度読んでから書き始める (読み手が動く前に書き終わって止めることがないように)。
                // 最初に読むのは前の画像なので、読み手がここより前に落ちることはない。
                first_read.wait();
                for round in 1..=rounds {
                    let start = std::sync::Barrier::new(WRITERS);
                    let results: Vec<Result<()>> = std::thread::scope(|writers| {
                        let handles: Vec<_> = (0..WRITERS)
                            .map(|writer| {
                                let (cache, start) = (&cache, &start);
                                writers.spawn(move || {
                                    let bytes = image(round, writer);
                                    start.wait();
                                    cache.write(url, &bytes)
                                })
                            })
                            .collect();
                        handles
                            .into_iter()
                            .map(|handle| handle.join().unwrap())
                            .collect()
                    });
                    // 失敗してよいのは、Windows が置き換えを断ったときだけ。
                    let mut written = Vec::new();
                    for (writer, result) in results.iter().enumerate() {
                        match result {
                            Ok(()) => written.push(writer),
                            Err(err) => assert!(
                                replace_refused(err, &path),
                                "{round} 回目の書き手 {writer}: {err:#}"
                            ),
                        }
                    }
                    // 残るのは、この回に書けたどれか 1 つの中身そのもの (どれも断られたら前の中身)。
                    let bytes = cache.read(url).unwrap();
                    let current = identify(&bytes);
                    if written.is_empty() {
                        assert_eq!(current, Some(kept), "{round} 回目に前の中身が残らなかった");
                    } else {
                        assert!(
                            current.is_some_and(
                                |(at, writer)| at == round && written.contains(&writer)
                            ),
                            "{round} 回目に、書けた中身でないもの ({} バイト) が残った",
                            bytes.len()
                        );
                    }
                    kept = current.unwrap();
                    succeeded += written.len();
                    // 書きかけは残らない。`.meta` は残った画像のもの (最後の書き込みが断られたときは、前の
                    // `.meta` を消したあとなのでない)。
                    assert_only_the_image(&cache, url);
                }
            }
            assert!(reader.join().unwrap() > 0, "一度も読めなかった");
        });
        assert!(succeeded > 0, "どの書き込みも成功しなかった");
    }

    #[test]
    fn concurrent_writes_to_one_url_leave_one_whole_image() {
        // 先読み (services) と画面 (http_client) は、同じ画像を同時に書くことがある。
        write_concurrently(5);
    }

    /// 重いので手で回す (`cargo test -p gnn-fetch --lib -- --ignored image_cache`)。
    #[test]
    #[ignore = "重い。手で回すストレステスト"]
    fn many_concurrent_writes_to_one_url_leave_one_whole_image() {
        write_concurrently(1000);
    }

    #[test]
    fn replaces_an_image_while_it_is_being_read() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ImageCache::new(dir.path().join("images"));
        let url = "https://example.com/a.png";
        let path = cache.path_for(url);
        cache.write(url, b"old").unwrap();
        // 読み手が開いている間 (画面の読み込みの途中) でも置き換えられる (Windows は std の rename が
        // POSIX の置き換えに切り替える)。読み手は前の中身を読み続け、新しく開くと新しい中身を読む。
        let mut reading = fs::File::open(&path).unwrap();
        // 書いたばかりのファイルはウイルス対策が検査していて、置き換えが断られることがあるので、少し待って
        // やり直す。読み手が開いているせいで断られるなら、何度やっても断られて落ちる。
        let mut attempts = 0;
        while let Err(err) = cache.write(url, b"new") {
            attempts += 1;
            assert!(attempts < 10 && replace_refused(&err, &path), "{err:#}");
            std::thread::sleep(Duration::from_millis(50));
        }
        let mut before = Vec::new();
        std::io::Read::read_to_end(&mut reading, &mut before).unwrap();
        assert_eq!(before, b"old");
        assert_eq!(cache.read(url).as_deref(), Some(&b"new"[..]));
        drop(reading);
        let image = file_name(&cache, url);
        assert_eq!(names(cache.dir()), [image.clone(), format!("{image}.meta")]);
        assert!(matches!(
            cache.read_checked(url).state,
            CacheState::Intact { bytes, .. } if bytes == b"new"
        ));
    }

    #[test]
    fn failed_writes_leave_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ImageCache::new(dir.path().join("images"));
        let url = "https://example.com/a.png";
        // 置き換える先が空でないディレクトリだと、置き換えに失敗する。
        let path = cache.path_for(url);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("inner"), b"x").unwrap();
        assert!(cache.write(url, b"aaaa").is_err());
        assert_eq!(names(cache.dir()), [file_name(&cache, url)]);
    }

    /// ほかのアプリが消すこと・名前を変えることを許さずに開いていると (共有モードに
    /// `FILE_SHARE_DELETE` がない)、置き換えに失敗する。そのときは前の中身が残り、書きかけは残らない。
    #[cfg(windows)]
    #[test]
    fn failed_replacements_keep_the_old_image() {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_SHARE_READ: u32 = 0x1;

        let dir = tempfile::tempdir().unwrap();
        let cache = ImageCache::new(dir.path().join("images"));
        let url = "https://example.com/a.png";
        cache.write(url, b"old").unwrap();
        let held = fs::File::options()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(cache.path_for(url))
            .unwrap();
        assert!(cache.write(url, b"new").is_err());
        drop(held);
        assert_eq!(cache.read(url).as_deref(), Some(&b"old"[..]));
        // 前の `.meta` は先に消したので、残った前の画像は「確かめていない」になる (壊れたとはしない)。
        assert_eq!(names(cache.dir()), [file_name(&cache, url)]);
        assert!(matches!(
            cache.read_checked(url).state,
            CacheState::Unverified { .. }
        ));
    }

    #[test]
    fn checks_cached_images_against_what_was_written() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ImageCache::new(dir.path().join("images"));
        let url = "https://cdn.example.com/a.png";
        assert_eq!(
            cache.read_checked(url),
            CacheRead {
                generation: 0,
                state: CacheState::Missing
            }
        );

        let whole = b"GIF89a-whole";
        cache.store(url, whole, &validators("\"a\"")).unwrap();
        assert_eq!(
            cache.read_checked(url),
            CacheRead {
                generation: cache.generation(url),
                state: CacheState::Intact {
                    bytes: whole.to_vec(),
                    sha256: sha256_hex(whole),
                    validators: validators("\"a\""),
                }
            }
        );

        // 書いたあとで壊れた: 途中で切れた・0 で埋まった (電源が落ちて書ききれなかった)・同じ長さで中身が違う。
        let path = cache.path_for(url);
        for broken in [&b"GIF89a-wh"[..], &[0u8; 12][..], &b"GIF89a-WHOLE"[..]] {
            fs::write(&path, broken).unwrap();
            assert_eq!(
                cache.read_checked(url).state,
                CacheState::Corrupted,
                "{broken:?}"
            );
        }

        // 照らすものがない: 前の版の `.meta` (検証子だけ)・`.meta` がない・壊れた `.meta`・形の違う SHA-256。
        // `.meta` のほうが壊れていても、画像が壊れたとは言えないので「確かめていない」にする。
        fs::write(&path, whole).unwrap();
        let meta = cache.meta_path_for(url);
        let unverified = CacheState::Unverified {
            bytes: whole.to_vec(),
            sha256: sha256_hex(whole),
        };
        for old in [
            Some(&br#"{"etag":"\"a\""}"#[..]),
            None,
            Some(&[0u8; 16][..]),
            Some(&br#"{"integrity":{"len":"twelve"}}"#[..]),
            Some(&br#"{"integrity":{"len":12,"sha256":"not-hex"}}"#[..]),
        ] {
            match old {
                Some(bytes) => fs::write(&meta, bytes).unwrap(),
                None => remove_if_exists(&meta).unwrap(),
            }
            assert_eq!(cache.read_checked(url).state, unverified, "{old:?}");
        }
        // 前の版の `.meta` の検証子は、今までどおり読める。
        fs::write(&meta, br#"{"etag":"\"a\""}"#).unwrap();
        assert_eq!(cache.validators(url).etag.as_deref(), Some("\"a\""));
    }

    #[test]
    fn rereads_under_the_lock_when_a_write_lands_between_the_files() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ImageCache::new(dir.path().join("images"));
        let url = "https://cdn.example.com/a.png";
        cache
            .store(url, b"GIF89a-old", &validators("\"a\""))
            .unwrap();
        // 読み手が前の `.meta` を読んだあとで、書き手が画像と `.meta` を入れ替える (読み手は新しい画像を読む)。
        let writer = cache.clone();
        *cache.read_pause.0.lock() = Some(Box::new(move || {
            writer
                .store(url, b"GIF89a-new!", &validators("\"b\""))
                .unwrap();
        }));
        // 食い違いは壊れたことにせず、ロックの中で読み直した新しい組を、その世代と一緒に返す。
        let read = cache.read_checked(url);
        assert_eq!(read.generation, cache.generation(url));
        assert!(
            matches!(
                &read.state,
                CacheState::Intact { bytes, validators: found, .. }
                    if bytes == b"GIF89a-new!" && *found == validators("\"b\"")
            ),
            "{read:?}"
        );

        // 前の版の画像 (`.meta` がない) を読んでいるあいだに入れ直された: 読み手は `.meta` がないと見たあとで
        // 新しい画像を読む (照らすものがないので、照らしても食い違いは分からない)。世代が進んだので読み直し、
        // 新しい組を返す。
        remove_if_exists(&cache.meta_path_for(url)).unwrap();
        let writer = cache.clone();
        *cache.read_pause.0.lock() = Some(Box::new(move || {
            writer
                .store(url, b"GIF89a-newer", &validators("\"c\""))
                .unwrap();
        }));
        let read = cache.read_checked(url);
        assert_eq!(read.generation, cache.generation(url));
        assert!(
            matches!(
                &read.state,
                CacheState::Intact { bytes, .. } if bytes == b"GIF89a-newer"
            ),
            "{read:?}"
        );

        // ほかのプロセスが画像だけを書き換えた途中のように (世代は進まない)、ロックの中で読み直しても合わなければ
        // 壊れたとする (捨てて取り直すだけになる)。
        let path = cache.path_for(url);
        *cache.read_pause.0.lock() = Some(Box::new(move || {
            fs::write(&path, b"GIF89a-other").unwrap();
        }));
        assert_eq!(cache.read_checked(url).state, CacheState::Corrupted);
    }

    #[test]
    fn discards_corrupted_images_with_their_meta() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ImageCache::new(dir.path().join("images"));
        let url = "https://cdn.example.com/a.png";
        let (path, meta) = (cache.path_for(url), cache.meta_path_for(url));
        cache.store(url, b"GIF89a-1", &validators("\"a\"")).unwrap();
        // 書いたときのままの画像・確かめていない画像は消さない。
        assert!(!cache.discard_corrupted(url).unwrap());
        remove_if_exists(&meta).unwrap();
        assert!(!cache.discard_corrupted(url).unwrap());
        assert!(path.is_file());

        cache.store(url, b"GIF89a-1", &validators("\"a\"")).unwrap();
        fs::write(&path, b"GIF89a-X").unwrap();
        let before = cache.generation(url);
        assert!(cache.discard_corrupted(url).unwrap());
        assert!(!path.exists() && !meta.exists());
        // 世代を進める (取りに行く前の世代で、遅れて届いた 304 を確認にしない)。
        assert_eq!(cache.generation(url), before + 1);
        assert!(!cache.discard_corrupted(url).unwrap());
    }

    /// ほかのプログラムが `.meta` を消すことを許さずに開いていても、画像を消せたら消せたことにする (画面が
    /// 読み直しを頼めるように)。残った `.meta` は画像がないので読まれず、次に入れるときに消す。
    #[cfg(windows)]
    #[test]
    fn discarding_counts_once_the_image_is_gone_even_if_the_meta_is_held() {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_SHARE_READ: u32 = 0x1;

        let dir = tempfile::tempdir().unwrap();
        let cache = ImageCache::new(dir.path().join("images"));
        let url = "https://cdn.example.com/a.png";
        cache.store(url, b"GIF89a-1", &validators("\"a\"")).unwrap();
        fs::write(cache.path_for(url), b"GIF89a-X").unwrap();
        let held = fs::File::options()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(cache.meta_path_for(url))
            .unwrap();
        let before = cache.generation(url);
        assert!(cache.discard_corrupted(url).unwrap());
        assert!(!cache.path_for(url).exists());
        assert!(cache.meta_path_for(url).exists());
        assert_eq!(cache.generation(url), before + 1);
        assert_eq!(cache.read_checked(url).state, CacheState::Missing);
        drop(held);
        cache
            .store(url, b"GIF89a-2", &Validators::default())
            .unwrap();
        assert!(matches!(
            cache.read_checked(url).state,
            CacheState::Intact { bytes, .. } if bytes == b"GIF89a-2"
        ));
    }

    #[test]
    fn discards_unverified_images_only_while_they_are_still_the_same() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ImageCache::new(dir.path().join("images"));
        let url = "https://cdn.example.com/a.png";
        let (path, meta) = (cache.path_for(url), cache.meta_path_for(url));
        fs::create_dir_all(cache.dir()).unwrap();
        // 前の版で書いた (`.meta` がない) 画像を読む。
        let unverified = |bytes: &[u8]| {
            remove_if_exists(&meta).unwrap();
            fs::write(&path, bytes).unwrap();
            let CacheRead { generation, state } = cache.read_checked(url);
            let CacheState::Unverified { sha256, .. } = state else {
                panic!("{state:?}");
            };
            (generation, sha256)
        };

        // 読んだあとで、このプロセスが入れ直した (世代が進む)。
        let (generation, sha256) = unverified(b"GIF89a-mixed");
        cache.write(url, b"GIF89a-fresh").unwrap();
        assert_eq!(
            cache.discard_unverified(url, generation, &sha256).unwrap(),
            Discard::Replaced
        );
        assert_eq!(cache.read(url).as_deref(), Some(&b"GIF89a-fresh"[..]));

        // 読んだあとで、ほかのプロセスが入れ直した (世代は同じで、中身が違う)。
        let (generation, sha256) = unverified(b"GIF89a-mixed");
        fs::write(&path, b"GIF89a-other").unwrap();
        assert_eq!(
            cache.discard_unverified(url, generation, &sha256).unwrap(),
            Discard::Replaced
        );
        assert!(path.is_file());

        // 読んだあとで、同じバイト列が配信元と同じと確かめられた (先読みが同じバイト列を取った)。取り直しても
        // 同じなので消さない。
        let (generation, sha256) = unverified(b"GIF89a-same");
        assert!(matches!(
            cache
                .confirm_if(
                    url,
                    generation,
                    &Validators::default(),
                    Integrity::of(b"GIF89a-same")
                )
                .unwrap(),
            Confirmed::Done
        ));
        assert_eq!(
            cache.discard_unverified(url, generation, &sha256).unwrap(),
            Discard::Verified
        );
        assert!(path.is_file() && meta.is_file());

        // 読んだときのままなら、画像と `.meta` を消して世代を進める。
        let (generation, sha256) = unverified(b"GIF89a-mixed");
        fs::write(&meta, br#"{"etag":"\"old\""}"#).unwrap();
        assert_eq!(
            cache.discard_unverified(url, generation, &sha256).unwrap(),
            Discard::Removed
        );
        assert!(!path.exists() && !meta.exists());
        assert_eq!(cache.generation(url), generation + 1);
        // もうない (prune など)。
        assert_eq!(
            cache
                .discard_unverified(url, generation + 1, &sha256)
                .unwrap(),
            Discard::Missing
        );
    }

    #[tokio::test]
    async fn revalidates_unchecked_and_corrupted_images_without_conditions() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ImageCache::new(dir.path().join("images"));
        let key = "https://source-image.invalid/gc/0123456789abcdef0123456789abcdef";
        let png = format!("Content-Type: {PNG}");
        let png = png.as_str();
        let (url, requests) = serve(vec![
            // 1: 前の版で書いた画像 (`.meta` は検証子だけ)。条件なしで取ると、同じバイト列だった。
            response("200 OK", &[png, "ETag: \"v1\""], "GIF89a-1"),
            // 2: 確かめた画像は、条件付きで取る。
            response("304 Not Modified", &["ETag: \"v1\""], ""),
            // 3: 書いたあとで壊れた画像は、条件なしで取り直す。
            response("200 OK", &[png, "ETag: \"v1\""], "GIF89a-1"),
            // 4: 前の版で混ざった画像 (`.meta` がない) は、条件なしで取り直して入れ直す。
            response("200 OK", &[png, "ETag: \"v2\""], "GIF89a-2"),
        ])
        .await;
        let client = reqwest::Client::new();
        let fetch = || fetch_image(&client, &cache, &url, key, 1024);

        fs::create_dir_all(cache.dir()).unwrap();
        fs::write(cache.path_for(key), b"GIF89a-1").unwrap();
        fs::write(cache.meta_path_for(key), br#"{"etag":"\"v0\""}"#).unwrap();
        // 1: 304 で延命しない。同じバイト列なら、配信元と同じと確かめたことになる。
        assert_eq!(fetch().await.unwrap(), Refreshed::Unchanged);
        assert!(!requests.lock()[0].contains("if-none-match"));
        assert!(matches!(
            cache.read_checked(key).state,
            CacheState::Intact { validators, .. } if validators.etag.as_deref() == Some("\"v1\"")
        ));

        // 2: 304 のあとも、書いた長さと SHA-256 を残す。
        assert_eq!(fetch().await.unwrap(), Refreshed::Unchanged);
        assert!(requests.lock()[1].contains("if-none-match: \"v1\""));
        assert!(matches!(
            cache.read_checked(key).state,
            CacheState::Intact { .. }
        ));

        // 3: 同じ長さで中身が違う (書いたあとで壊れた)。
        fs::write(cache.path_for(key), b"GIF89a-X").unwrap();
        assert_eq!(fetch().await.unwrap(), Refreshed::Changed);
        assert!(!requests.lock()[2].contains("if-none-match"));
        assert!(matches!(
            cache.read_checked(key).state,
            CacheState::Intact { bytes, .. } if bytes == b"GIF89a-1"
        ));

        // 4
        remove_if_exists(&cache.meta_path_for(key)).unwrap();
        fs::write(cache.path_for(key), b"GIF89a-1-and-2").unwrap();
        assert_eq!(fetch().await.unwrap(), Refreshed::Changed);
        assert!(!requests.lock()[3].contains("if-none-match"));
        assert!(matches!(
            cache.read_checked(key).state,
            CacheState::Intact { bytes, .. } if bytes == b"GIF89a-2"
        ));
    }
}
