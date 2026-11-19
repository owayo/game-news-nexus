//! HTTP 取得。文字コードの判定、ホストごとの同時接続数の制限、再試行を受け持つ。

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use bytes::Bytes;
use gnn_core::Secrets;
use gnn_core::net;
use gnn_core::source::HttpMethod;
use parking_lot::Mutex;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, RETRY_AFTER};
use tokio::sync::Semaphore;
use url::Url;

/// 既定の User-Agent。ブラウザ以外を拒む配信元があるため、一般的なブラウザに合わせる。
pub const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36";

/// 1 つのホストに同時に送るリクエストの上限。
const PER_HOST_LIMIT: usize = 2;
/// 応答の大きさの上限。
const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;
/// たどるリダイレクトの上限。
const MAX_REDIRECTS: usize = 10;

/// 取得先の制限。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum NetworkPolicy {
    /// どこへでも取得する。利用者が自分で書いたソースに使う。
    #[default]
    Any,
    /// インターネット上だけに取得する。カタログのソース (第三者の PR 由来) と、
    /// 手元のネットワークを許していない手元のソースに使う。
    ///
    /// localhost・LAN・リンクローカルなどへは、名前解決した後のアドレスとリダイレクト先も
    /// 含めて接続しない。プロキシも使わない (プロキシ先での名前解決を確かめられないため)。
    PublicOnly,
}

#[derive(Debug, Clone)]
pub struct HttpOptions {
    pub user_agent: String,
    pub timeout: Duration,
}

impl Default for HttpOptions {
    fn default() -> Self {
        Self {
            user_agent: DEFAULT_USER_AGENT.to_string(),
            timeout: Duration::from_secs(30),
        }
    }
}

/// 取得した応答。
#[derive(Debug, Clone)]
pub struct Fetched {
    /// リダイレクト後の URL。相対 URL の基準になる。
    pub url: Url,
    pub content_type: Option<String>,
    pub bytes: Bytes,
}

impl Fetched {
    /// 文字列にする。`forced` が指定されていればその文字コードで読む。
    pub fn text(&self, forced: Option<&str>) -> String {
        decode(
            &self.bytes,
            self.content_type.as_deref(),
            forced,
            self.url.host_str(),
        )
    }
}

/// HTTP クライアント。複製しても同じ接続プールと Cookie を共有する。
#[derive(Clone)]
pub struct HttpFetcher {
    client: reqwest::Client,
    /// [`NetworkPolicy::PublicOnly`] の取得に使うクライアント。
    public_client: reqwest::Client,
    hosts: Arc<Mutex<HashMap<String, Arc<Semaphore>>>>,
    secrets: Arc<Secrets>,
    options: HttpOptions,
}

impl HttpFetcher {
    pub fn new(options: HttpOptions, secrets: Secrets) -> Result<Self> {
        Ok(Self {
            client: build_client(&options, NetworkPolicy::Any)?,
            public_client: build_client(&options, NetworkPolicy::PublicOnly)?,
            hosts: Arc::default(),
            secrets: Arc::new(secrets),
            options,
        })
    }

    /// Cookie を持たない新しいクライアントで取得するものを作る。ホストごとの同時接続数の
    /// 制限と秘密の値は共有する。
    ///
    /// ソースごとに分けて使う。あるソース (秘密の値で認証した手元の定義など) が受け取った
    /// Cookie を、ほかのソース (カタログの定義) が送ってしまわないようにするため。
    pub fn isolated(&self) -> Result<Self> {
        Ok(Self {
            client: build_client(&self.options, NetworkPolicy::Any)?,
            public_client: build_client(&self.options, NetworkPolicy::PublicOnly)?,
            hosts: self.hosts.clone(),
            secrets: self.secrets.clone(),
            options: self.options.clone(),
        })
    }

    /// 制限に合うクライアント。画像の取得など、エンジンの外からも使う。
    pub fn client(&self, policy: NetworkPolicy) -> &reqwest::Client {
        match policy {
            NetworkPolicy::Any => &self.client,
            NetworkPolicy::PublicOnly => &self.public_client,
        }
    }

    fn host_permit(&self, url: &Url) -> Arc<Semaphore> {
        let host = url.host_str().unwrap_or_default().to_string();
        self.hosts
            .lock()
            .entry(host)
            .or_insert_with(|| Arc::new(Semaphore::new(PER_HOST_LIMIT)))
            .clone()
    }

    /// GET する。
    pub async fn get(
        &self,
        policy: NetworkPolicy,
        url: &str,
        headers: &BTreeMap<String, String>,
    ) -> Result<Fetched> {
        self.send(policy, HttpMethod::Get, url, headers, None).await
    }

    /// リクエストを送る。429 と 503 は 1 度だけ待って再試行する。
    pub async fn send(
        &self,
        policy: NetworkPolicy,
        method: HttpMethod,
        url: &str,
        headers: &BTreeMap<String, String>,
        body: Option<&str>,
    ) -> Result<Fetched> {
        let url = Url::parse(url).with_context(|| format!("URL `{url}` を解釈できません"))?;
        if !matches!(url.scheme(), "http" | "https") {
            bail!("http / https 以外の URL は取得できません: {url}");
        }
        if policy == NetworkPolicy::PublicOnly {
            net::check_public_url(&url).map_err(|message| anyhow!(message))?;
        }
        let client = self.client(policy);
        let mut header_map = HeaderMap::new();
        for (name, value) in headers {
            let value = self.secrets.resolve(value)?;
            let name = HeaderName::from_bytes(name.as_bytes())
                .with_context(|| format!("ヘッダ名 `{name}` が不正です"))?;
            let value = HeaderValue::from_str(&value)
                .with_context(|| format!("ヘッダ `{name}` の値が不正です"))?;
            header_map.insert(name, value);
        }

        let permit = self.host_permit(&url);
        let _permit = permit
            .acquire()
            .await
            .map_err(|_| anyhow!("取得を中止しました"))?;

        let mut attempt = 0;
        loop {
            attempt += 1;
            let request = match method {
                HttpMethod::Get => client.get(url.clone()),
                HttpMethod::Post => client
                    .post(url.clone())
                    .body(body.unwrap_or_default().to_string()),
            }
            .headers(header_map.clone());

            let response = request
                .send()
                .await
                .map_err(|err| describe_error(err, &url))?;
            let status = response.status();
            if (status.as_u16() == 429 || status.as_u16() == 503) && attempt == 1 {
                let wait = response
                    .headers()
                    .get(RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(3)
                    .min(30);
                tokio::time::sleep(Duration::from_secs(wait)).await;
                continue;
            }
            if !status.is_success() {
                return Err(HttpStatus {
                    status: status.as_u16(),
                    url: url.to_string(),
                }
                .into());
            }

            let final_url = response.url().clone();
            let content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            let bytes = read_capped(response, MAX_BODY_BYTES)
                .await
                .with_context(|| format!("{url} の応答を読めません"))?;
            return Ok(Fetched {
                url: final_url,
                content_type,
                bytes,
            });
        }
    }
}

/// 2xx 以外の応答。取得の誤り (`anyhow::Error`) から [`http_status`] で取り出せる。
#[derive(Debug, thiserror::Error)]
#[error("HTTP {status} ({url})")]
pub struct HttpStatus {
    pub status: u16,
    pub url: String,
}

/// 取得の誤りの原因に 2xx 以外の応答があれば、その状態コード。
pub fn http_status(err: &anyhow::Error) -> Option<u16> {
    err.chain()
        .find_map(|cause| cause.downcast_ref::<HttpStatus>())
        .map(|status| status.status)
}

/// 応答の本文を、受け取りながら累計の大きさで打ち切って読む。
///
/// Content-Length がない応答や、圧縮を展開すると大きくなる応答でも、上限を超えた
/// 時点で読むのをやめる。
pub async fn read_capped(mut response: reqwest::Response, max: usize) -> Result<Bytes> {
    if response
        .content_length()
        .is_some_and(|length| length as usize > max)
    {
        bail!("応答が大きすぎます (上限 {} MB)", max / (1024 * 1024));
    }
    let mut buffer = bytes::BytesMut::new();
    while let Some(chunk) = response.chunk().await? {
        if buffer.len() + chunk.len() > max {
            bail!("応答が大きすぎます (上限 {} MB)", max / (1024 * 1024));
        }
        buffer.extend_from_slice(&chunk);
    }
    Ok(buffer.freeze())
}

fn build_client(options: &HttpOptions, policy: NetworkPolicy) -> Result<reqwest::Client> {
    let mut headers = HeaderMap::new();
    headers.insert(
        reqwest::header::ACCEPT_LANGUAGE,
        HeaderValue::from_static("ja,en-US;q=0.8,en;q=0.6"),
    );
    let builder = reqwest::Client::builder()
        .user_agent(options.user_agent.clone())
        .default_headers(headers)
        .timeout(options.timeout)
        .connect_timeout(Duration::from_secs(15))
        .cookie_store(true);
    let builder = match policy {
        NetworkPolicy::Any => builder.redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS)),
        NetworkPolicy::PublicOnly => builder
            .no_proxy()
            .dns_resolver(Arc::new(PublicResolver) as Arc<dyn reqwest::dns::Resolve>)
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= MAX_REDIRECTS {
                    return attempt.error(format!("リダイレクトが {MAX_REDIRECTS} 回を超えました"));
                }
                match net::check_public_url(attempt.url()) {
                    Ok(()) => attempt.follow(),
                    Err(message) => attempt.error(message),
                }
            })),
    };
    builder.build().context("HTTP クライアントを作れません")
}

/// 名前解決の結果から、手元のネットワークのアドレスを除く。
///
/// 公開の名前が LAN のアドレスを返す (DNS リバインディングなど) 場合にも、
/// 手元のネットワークへは接続しない。
struct PublicResolver;

impl reqwest::dns::Resolve for PublicResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            if net::is_local_name(&host) {
                return Err(format!("手元のネットワークの名前 {host} には接続しません").into());
            }
            let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|address| net::is_public_ip(address.ip()))
                .collect();
            if addresses.is_empty() {
                return Err(format!(
                    "{host} は手元のネットワークのアドレスを指しているため接続しません"
                )
                .into());
            }
            Ok(Box::new(addresses.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

fn describe_error(err: reqwest::Error, url: &Url) -> anyhow::Error {
    if err.is_timeout() {
        anyhow!("時間切れ ({url})")
    } else if err.is_connect() {
        anyhow!("接続できません ({url}): {err}")
    } else {
        anyhow!("通信エラー ({url}): {err}")
    }
}

/// 応答の文字コードを決めて文字列にする。
///
/// 指定 → Content-Type → BOM → HTML の meta → UTF-8 として正しいか → 推定 の順に決める。
pub fn decode(
    bytes: &[u8],
    content_type: Option<&str>,
    forced: Option<&str>,
    host: Option<&str>,
) -> String {
    let label = forced
        .map(str::to_string)
        .or_else(|| content_type.and_then(charset_param))
        .or_else(|| sniff_meta_charset(bytes));
    if let Some(encoding) = label
        .as_deref()
        .and_then(|label| encoding_rs::Encoding::for_label(label.trim().as_bytes()))
    {
        let (text, _, _) = encoding.decode(bytes);
        return text.into_owned();
    }
    if let Some((encoding, _)) = encoding_rs::Encoding::for_bom(bytes) {
        let (text, _, _) = encoding.decode(bytes);
        return text.into_owned();
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_string();
    }
    let mut detector = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Allow);
    detector.feed(bytes, true);
    let tld = host
        .and_then(|host| host.rsplit('.').next())
        .map(str::as_bytes);
    let encoding = detector.guess(tld, chardetng::Utf8Detection::Allow);
    let (text, _, _) = encoding.decode(bytes);
    text.into_owned()
}

fn charset_param(content_type: &str) -> Option<String> {
    content_type.split(';').skip(1).find_map(|part| {
        let (key, value) = part.split_once('=')?;
        key.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| value.trim().trim_matches('"').to_string())
    })
}

fn sniff_meta_charset(bytes: &[u8]) -> Option<String> {
    let head = &bytes[..bytes.len().min(4096)];
    let head = String::from_utf8_lossy(head).to_ascii_lowercase();
    let index = head.find("charset=")?;
    let rest = head[index + "charset=".len()..].trim_start_matches(['"', '\'']);
    let end = rest
        .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'))
        .unwrap_or(rest.len());
    let label = &rest[..end];
    (!label.is_empty()).then(|| label.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_by_header_meta_and_detection() {
        let (sjis, _, _) = encoding_rs::SHIFT_JIS.encode("メンテナンスのお知らせ");
        assert_eq!(
            decode(&sjis, Some("text/html; charset=Shift_JIS"), None, None),
            "メンテナンスのお知らせ"
        );
        assert_eq!(
            decode(&sjis, None, Some("shift_jis"), None),
            "メンテナンスのお知らせ"
        );

        let mut html = b"<html><head><meta charset=\"shift_jis\"></head><body>".to_vec();
        html.extend_from_slice(&sjis);
        assert!(decode(&html, Some("text/html"), None, None).ends_with("メンテナンスのお知らせ"));

        assert_eq!(
            decode("お知らせ".as_bytes(), Some("application/json"), None, None),
            "お知らせ"
        );
        let (euc, _, _) =
            encoding_rs::EUC_JP.encode("イベント開催のお知らせです。詳細は以下をご確認ください。");
        assert_eq!(
            decode(&euc, None, None, Some("example.jp")),
            "イベント開催のお知らせです。詳細は以下をご確認ください。"
        );
    }

    #[test]
    fn reads_charset_parameter() {
        assert_eq!(
            charset_param("text/html; charset=\"UTF-8\"").as_deref(),
            Some("UTF-8")
        );
        assert_eq!(charset_param("application/json"), None);
    }
}
