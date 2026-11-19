//! 取得定義に従って記事を取得する。
//!
//! `prepare (変数) → lists (一覧・ページ送り) → 候補の統合と選別 → detail (詳細) → body (整形)`
//! の順に進む。保存はしない。結果と診断を返し、保存は呼び出し側 (ストア) が行う。

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Context as _, Result, anyhow, bail};
use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use futures::StreamExt as _;
use gnn_core::entry::list_signature;
use gnn_core::source::{
    BindSpec, BodyFormat, BodySpec, DEFAULT_DETAIL_URL, DetailSpec, DocumentKind, FeedContent,
    HttpMethod, JsonBody, JsonFields, ListSpec, MAX_PAGES, PagesSpec, PrepareStep, RequestSpec,
    SourceConfig, UrlIdentity,
};
use gnn_core::store::KnownItem;
use gnn_core::text::{decode_entities, escape_html, html_to_text};
use gnn_core::{EntryDraft, ImageIdentity, Published, RunStatus};
use regex::Regex;
use scraper::{Html, Selector};
use serde_json::Value;
use url::Url;

use crate::clean::{CleanOptions, clean_html};
use crate::dates::{self, DateContext};
use crate::html::{
    CompiledFields, CompiledValue, body_inner_html, collapse_whitespace, remove_elements, selector,
};
use crate::http::{Fetched, HttpFetcher, NetworkPolicy};
use crate::json::{eval, matches, pointer, scalar_to_string};
use crate::template::{Escape, ItemValues, Scope, render};

/// 1 回に読む記事の上限の既定値。
pub const DEFAULT_MAX_ITEMS: usize = 100;
/// 同じソースで同時に取得する詳細ページの数。
const DETAIL_CONCURRENCY: usize = 3;

/// 取得の入力。
pub struct RunInput<'a> {
    pub source: &'a SourceConfig,
    /// 保存済みの記事。外部 ID ごと。
    pub known: &'a HashMap<String, KnownItem>,
    /// これより古い記事は読まない (日)。0 は無制限。
    pub max_age_days: u32,
    pub now: DateTime<Utc>,
    /// 投稿からこの時間内の記事は、毎回詳細を取り直す (追記に追いつくため)。
    /// 投稿日時が分からない記事は、初めて見た時刻から数える。
    pub recheck: Duration,
    /// 投稿からの時間によらず、詳細を取り直す記事の外部 ID。どの記事を取り直すかは呼び出し側が
    /// 決める (アプリは予定の時間が近いメンテナンスの記事と、取得 URL で取れなかった本文の画像を持つ
    /// 記事を渡す)。取得する期間 (`max_age_days`) より古くても読む。一覧のページ送りも、これらの記事が
    /// 見つかるまでは取得済みの記事だけのページで止めない。一覧に見つからなければ、保存済みの値
    /// (`known` の URL・題名など) から詳細を取る (一覧そのものを置き換えるソースを除く)。
    pub recheck_ids: HashSet<String>,
    /// 記事数の上限を上書きする。テスト取得で使う。
    pub limit: Option<usize>,
    /// 取得先の制限。カタログのソースは [`NetworkPolicy::PublicOnly`] にする。
    pub network: NetworkPolicy,
    /// ヘッダの `{{secret:名前}}` を使えるか。カタログのソースでは使えない。
    pub allow_secrets: bool,
    /// 段ごとの記録を残す。テスト取得で使う。
    pub trace: bool,
}

/// 取得の結果。
#[derive(Debug, Clone)]
pub struct RunOutput {
    pub drafts: Vec<EntryDraft>,
    /// 一覧で見つけた記事の数 (重複を除く)。
    pub listed: usize,
    /// 一覧で見つけたすべての記事の ID。上限や期間で読まなかった分も含む。
    /// 一覧そのものを置き換えるソースで、一覧に残っている記事を消さないために使う。
    pub listed_ids: Vec<String>,
    pub status: RunStatus,
    pub message: Option<String>,
    /// 一部の一覧・ページ・記事の失敗など。
    pub warnings: Vec<String>,
    /// 段ごとの記録。[`RunInput::trace`] のときだけ。
    pub trace: Vec<TraceStep>,
    /// 詳細を取り直す記事 ([`RunInput::recheck_ids`]) のうち、詳細を取れなかったもの (外部 ID ごと)。
    pub recheck_misses: HashMap<String, RecheckMiss>,
}

/// 詳細を取り直す記事の詳細を取れなかった理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecheckMiss {
    /// 一覧になく、保存済みの値に詳細を取る URL もない (一覧を置き換えるソースでは、一覧にない記事は取らない)。
    NotFound,
    /// 詳細が 404・410 だった (記事が消えた)。
    Gone,
    /// 詳細を取れなかった (通信の失敗・5xx・429 など。時間をおけば取れるかもしれない)。
    Failed,
}

impl RecheckMiss {
    fn of(err: &anyhow::Error) -> Self {
        match crate::http::http_status(err) {
            Some(404 | 410) => RecheckMiss::Gone,
            _ => RecheckMiss::Failed,
        }
    }
}

impl RunOutput {
    /// 一覧から取れなかった (取得そのものの失敗)。
    pub fn failed(err: anyhow::Error, trace: Vec<TraceStep>) -> Self {
        Self {
            drafts: Vec::new(),
            listed: 0,
            listed_ids: Vec::new(),
            status: RunStatus::Failed,
            message: Some(format!("{err:#}")),
            warnings: Vec::new(),
            trace,
            recheck_misses: HashMap::new(),
        }
    }
}

/// 処理の段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Prepare,
    List,
    Merge,
    Detail,
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Stage::Prepare => "事前の取得",
            Stage::List => "一覧",
            Stage::Merge => "まとめ",
            Stage::Detail => "詳細",
        }
    }
}

/// 段ごとの記録。テスト取得で、どこで何が起きたかを見せる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceStep {
    pub stage: Stage,
    pub title: String,
    pub detail: String,
    pub ok: bool,
}

#[derive(Default)]
struct Trace {
    enabled: bool,
    steps: Vec<TraceStep>,
}

impl Trace {
    fn push(
        &mut self,
        stage: Stage,
        title: impl Into<String>,
        detail: impl Into<String>,
        ok: bool,
    ) {
        if self.enabled {
            self.steps.push(TraceStep {
                stage,
                title: title.into(),
                detail: detail.into(),
                ok,
            });
        }
    }
}

/// 一覧から読んだ 1 件。
#[derive(Debug, Clone, Default)]
struct ListItem {
    id: Option<String>,
    title: Option<String>,
    url: Option<String>,
    published_raw: Option<String>,
    published: Option<Published>,
    body: Option<String>,
    image: Option<String>,
    note: Option<String>,
    json: Option<Value>,
}

impl ListItem {
    /// 保存済みの記事の値から作る。一覧に見つからなかった、詳細を取り直す記事に使う。
    fn from_known(external_id: &str, known: &KnownItem) -> Self {
        Self {
            id: Some(external_id.to_string()),
            title: Some(known.title.clone()),
            url: known.url.clone(),
            published: known.published,
            image: known.image_url.clone(),
            note: known.note.clone(),
            ..Self::default()
        }
    }
}

/// 1 ページ分の一覧。
struct Page {
    items: Vec<ListItem>,
    /// 絞り込む前の件数。ページの終わりの判断に使う
    /// (条件で全件を除いたページでも、次のページに記事があることがある)。
    raw: usize,
    /// 次のページの URL (`next_link`) かカーソル (`cursor`)。
    next: Option<String>,
}

/// 詳細ページから読んだ値。
#[derive(Debug, Default)]
struct DetailValues {
    title: Option<String>,
    published: Option<Published>,
    body: Option<String>,
    image: Option<String>,
    note: Option<String>,
    base: Option<Url>,
}

/// 組み立てたリクエスト。
struct Request {
    method: HttpMethod,
    url: Url,
    headers: BTreeMap<String, String>,
    body: Option<String>,
}

/// 解釈済みの本文の整え方。
struct CompiledBody<'a> {
    spec: &'a BodySpec,
    replace: Vec<(Regex, String)>,
    remove: Vec<Selector>,
    base_url: Option<Url>,
}

impl<'a> CompiledBody<'a> {
    fn compile(spec: &'a BodySpec) -> Result<Self> {
        Ok(Self {
            spec,
            replace: spec
                .replace
                .iter()
                .map(|rule| {
                    Regex::new(&rule.pattern)
                        .map(|regex| (regex, rule.with.clone()))
                        .with_context(|| {
                            format!("置換の正規表現 `{}` が誤っています", rule.pattern)
                        })
                })
                .collect::<Result<_>>()?,
            remove: spec
                .remove
                .iter()
                .map(|text| selector(text))
                .collect::<Result<_>>()?,
            base_url: spec
                .base_url
                .as_deref()
                .and_then(|url| Url::parse(url).ok()),
        })
    }

    /// テキストなら逃がす → 置換 → 画像を先頭に → 要素を除く → 安全な HTML にして整える。
    fn finish(&self, raw: Option<String>, image: Option<&str>, base: &Url) -> String {
        let mut body = raw.unwrap_or_default();
        if self.spec.format == BodyFormat::Text {
            body = escape_html(&body);
        }
        for (regex, with) in &self.replace {
            body = regex.replace_all(&body, with.as_str()).into_owned();
        }
        if self.spec.image_in_body
            && let Some(image) = image
        {
            body = format!("<p><img src=\"{}\"></p>{body}", escape_html(image));
        }
        body = remove_elements(&body, &self.remove);
        clean_html(
            &body,
            CleanOptions {
                base_url: Some(self.base_url.as_ref().unwrap_or(base)),
                newlines_to_br: self.spec.newlines_to_br || self.spec.format == BodyFormat::Text,
                tidy: self.spec.tidy,
            },
        )
    }
}

/// 取得エンジン。
#[derive(Clone)]
pub struct Engine {
    http: HttpFetcher,
}

impl Engine {
    pub fn new(http: HttpFetcher) -> Self {
        Self { http }
    }

    pub fn http(&self) -> &HttpFetcher {
        &self.http
    }

    /// Cookie を共有しない新しいエンジン。1 つのソースの取得ごとに使う
    /// ([`HttpFetcher::isolated`])。
    pub fn isolated(&self) -> Result<Self> {
        Ok(Self {
            http: self.http.isolated()?,
        })
    }

    /// 1 つのソースを取得する。失敗も結果として返す。
    pub async fn run(&self, input: RunInput<'_>) -> RunOutput {
        let mut trace = Trace {
            enabled: input.trace,
            steps: Vec::new(),
        };
        match self.run_inner(&input, &mut trace).await {
            Ok(mut output) => {
                output.trace = trace.steps;
                output
            }
            Err(err) => RunOutput::failed(err, trace.steps),
        }
    }

    async fn run_inner(&self, input: &RunInput<'_>, trace: &mut Trace) -> Result<RunOutput> {
        let source = input.source;
        let tz: Tz = source
            .timezone
            .as_deref()
            .map(|tz| {
                tz.parse()
                    .map_err(|_| anyhow!("タイムゾーン `{tz}` を解釈できません"))
            })
            .transpose()?
            .unwrap_or(chrono_tz::Asia::Tokyo);
        let ctx = DateContext { tz, now: input.now };
        let body = CompiledBody::compile(&source.body)?;
        let signing = Signing::of(source);
        let cutoff =
            (input.max_age_days > 0).then(|| input.now - Duration::days(input.max_age_days.into()));

        // 1. 事前の取得で変数を作る。どれかが失敗すれば続けられない。
        let mut vars: BTreeMap<String, String> = BTreeMap::new();
        for (index, step) in source.prepare.iter().enumerate() {
            let title = format!("{} 番目", index + 1);
            match self.prepare(step, &vars, input).await {
                Ok((url, bound)) => {
                    let values: Vec<String> = bound
                        .iter()
                        .map(|(name, value)| format!("{name} = {value}"))
                        .collect();
                    trace.push(
                        Stage::Prepare,
                        title,
                        format!("{url} → {}", values.join(", ")),
                        true,
                    );
                    vars.extend(bound);
                }
                Err(err) => {
                    trace.push(Stage::Prepare, title, format!("{err:#}"), false);
                    return Err(err.context(format!("事前の取得 ({} 番目)", index + 1)));
                }
            }
        }

        // 2. 一覧を宣言順に読む。一部の一覧が失敗しても、ほかの一覧の記事は取り込む。
        let mut warnings = Vec::new();
        let mut list_errors = Vec::new();
        let mut items: Vec<(ListItem, Url)> = Vec::new();
        for (index, list) in source.lists.iter().enumerate() {
            match self
                .read_list(list, index, &vars, ctx, input, cutoff, trace)
                .await
            {
                Ok((list_items, list_warnings)) => {
                    items.extend(list_items);
                    warnings.extend(list_warnings);
                }
                Err(err) => {
                    trace.push(Stage::List, list.label(index), format!("{err:#}"), false);
                    list_errors.push(format!("{}: {err:#}", list.label(index)));
                }
            }
        }
        if !list_errors.is_empty() && list_errors.len() == source.lists.len() {
            bail!("{}", list_errors.join(" / "));
        }
        warnings.extend(list_errors);

        // 3. 記事を見分け、重複を除く。同じ記事は先の一覧の値を使う。
        let mut seen = HashSet::new();
        let mut candidates = Vec::new();
        let total = items.len();
        let mut unidentified = 0;
        for (mut item, base) in items {
            item.url = item.url.as_deref().and_then(|url| resolve(url, &base));
            item.image = item
                .image
                .as_deref()
                .and_then(|image| resolve(&decode_entities(image), &base));
            let Some(external_id) = external_id(&item) else {
                unidentified += 1;
                continue;
            };
            if !seen.insert(external_id.clone()) {
                continue;
            }
            candidates.push((external_id, item, base));
        }
        if total > 0 && candidates.is_empty() {
            bail!(
                "一覧の {total} 件のどれからも ID・URL・タイトルを読めません (ページの構造が変わった可能性があります)"
            );
        }
        if unidentified > 0 {
            warnings.push(format!(
                "一覧の {unidentified} 件から ID・URL・タイトルを読めませんでした"
            ));
        }
        let listed = candidates.len();
        let listed_ids: Vec<String> = candidates.iter().map(|(id, _, _)| id.clone()).collect();

        // 取り直す記事のうち、一覧に見つからなかったもの (ページ送りの外・一覧から外れた記事) は、保存済みの
        // 値 (URL・題名など) から詳細を取る。一覧そのものを置き換えるソースでは行わない (一覧にない記事を
        // 残してしまうため)。詳細を取れなくても (消えた記事など)、ソースの失敗には数えない。
        let mut revisits: HashSet<String> = HashSet::new();
        let mut recheck_misses: HashMap<String, RecheckMiss> = HashMap::new();
        let mut unseen: Vec<&String> = input
            .recheck_ids
            .iter()
            .filter(|id| !seen.contains(id.as_str()))
            .collect();
        unseen.sort();
        if !unseen.is_empty() {
            let revisit = source.detail.is_some();
            for external_id in unseen {
                let stored = input
                    .known
                    .get(external_id)
                    .filter(|_| revisit)
                    .and_then(|known| {
                        let base = Url::parse(known.url.as_deref()?).ok()?;
                        Some((known, base))
                    });
                let Some((known, base)) = stored else {
                    recheck_misses.insert(external_id.clone(), RecheckMiss::NotFound);
                    continue;
                };
                candidates.push((
                    external_id.clone(),
                    ListItem::from_known(external_id, known),
                    base,
                ));
                revisits.insert(external_id.clone());
            }
            if !revisits.is_empty() {
                trace.push(
                    Stage::Merge,
                    "一覧にない取り直す記事",
                    format!("{} 件は保存済みの URL から詳細を取る", revisits.len()),
                    true,
                );
            }
        }

        let limit = input
            .limit
            .or(source.max_items)
            .unwrap_or(DEFAULT_MAX_ITEMS);
        // 上限は、新しい・変わった記事だけで数える。保存済みの値を使うだけの記事で枠を使うと、後の一覧の
        // 新着がいつまでも取り込めなくなるため。呼び出し側が指示した取り直す記事 (`recheck_ids`) は、上限にも
        // 取得する期間にもよらず読む (新着が上限を使い切ると、取り直せなくなるため)。
        let mut needing_work = 0;
        let candidates: Vec<_> = candidates
            .into_iter()
            .filter(|(external_id, item, _)| {
                input.recheck_ids.contains(external_id) || !is_too_old(item.published, cutoff)
            })
            .filter(|(external_id, item, _)| {
                if input.limit.is_none() && input.recheck_ids.contains(external_id) {
                    return true;
                }
                let reuse = input.limit.is_none()
                    && input.known.get(external_id).is_some_and(|known| {
                        !needs_work(source, input, external_id, item, known, &signing)
                    });
                if reuse {
                    return true;
                }
                needing_work += 1;
                needing_work <= limit
            })
            .collect();
        trace.push(
            Stage::Merge,
            "記事の候補",
            format!(
                "{total} 件から重複を除いて {listed} 件、期間と上限で {} 件を読む",
                candidates.len()
            ),
            true,
        );

        // 4. 必要なら詳細を取り、本文を整える。
        let detail_fields = match &source.detail {
            Some(DetailSpec::Html(detail)) => Some(CompiledFields::compile(&detail.fields, &vars)?),
            _ => None,
        };
        let kinds: Vec<(String, bool)> = candidates
            .iter()
            .map(|(external_id, _, _)| (external_id.clone(), revisits.contains(external_id)))
            .collect();
        let results: Vec<Result<(Option<EntryDraft>, bool)>> =
            futures::stream::iter(candidates.into_iter().map(|(external_id, item, base)| {
                let vars = &vars;
                let body = &body;
                let signing = &signing;
                let detail_fields = detail_fields.as_ref();
                let revisit = revisits.contains(&external_id);
                async move {
                    self.build_draft(
                        input,
                        ctx,
                        external_id,
                        item,
                        &base,
                        vars,
                        body,
                        detail_fields,
                        cutoff,
                        signing,
                        revisit,
                    )
                    .await
                }
            }))
            .buffered(DETAIL_CONCURRENCY)
            .collect()
            .await;

        let mut drafts = Vec::new();
        let mut fetched_details = 0;
        let mut detail_failures = 0;
        for (result, (external_id, revisit)) in results.into_iter().zip(kinds) {
            match result {
                Ok((draft, fetched)) => {
                    fetched_details += usize::from(fetched);
                    drafts.extend(draft);
                }
                Err(err) => {
                    if input.recheck_ids.contains(&external_id) {
                        recheck_misses.insert(external_id, RecheckMiss::of(&err));
                    }
                    if revisit {
                        // 一覧にない記事の詳細は、消えていることがある。ソースの失敗にはしない。
                        trace.push(Stage::Detail, "一覧にない記事", format!("{err:#}"), false);
                        tracing::debug!(source = %source.id, "一覧にない記事の詳細を取れません: {err:#}");
                    } else {
                        detail_failures += 1;
                        warnings.push(format!("{err:#}"));
                    }
                }
            }
        }
        if source.detail.is_some() {
            trace.push(
                Stage::Detail,
                "詳細ページ",
                format!(
                    "{fetched_details} 件を取得、{} 件は保存済みの値を使用{}",
                    drafts.len().saturating_sub(fetched_details),
                    if detail_failures > 0 {
                        format!("、{detail_failures} 件は失敗")
                    } else {
                        String::new()
                    }
                ),
                detail_failures == 0,
            );
        }

        let status = if warnings.is_empty() {
            RunStatus::Ok
        } else {
            RunStatus::Partial
        };
        let message = (!warnings.is_empty()).then(|| match warnings.len() {
            1 => warnings[0].clone(),
            count => format!("{count} 件の問題があります: {}", warnings[0]),
        });
        Ok(RunOutput {
            drafts,
            listed,
            listed_ids,
            status,
            message,
            warnings,
            trace: Vec::new(),
            recheck_misses,
        })
    }

    /// 事前の取得。取り出した値を返す。
    async fn prepare(
        &self,
        step: &PrepareStep,
        vars: &BTreeMap<String, String>,
        input: &RunInput<'_>,
    ) -> Result<(String, Vec<(String, String)>)> {
        let scope = Scope {
            vars: Some(vars),
            ..Scope::default()
        };
        let request = build_request(&step.request, &scope, None)?;
        let url = request.url.to_string();
        let fetched = self.send(input, request).await?;
        let text = fetched.text(step.request.encoding.as_deref());
        let mut bound = Vec::new();
        for (name, value) in &step.bind {
            let spec = value.spec(step.kind);
            let extracted = extract_bind(step.kind, &text, &spec)
                .with_context(|| format!("変数 `{name}` を取り出せません"))?;
            bound.push((name.clone(), extracted));
        }
        Ok((url, bound))
    }

    /// 1 つの一覧を、ページ送りを含めて読む。記事と、その記事を読んだページの URL を返す。
    /// 最初のページの失敗は一覧の失敗、2 ページ目以降の失敗は警告にしてそこで止める。
    #[allow(clippy::too_many_arguments)]
    async fn read_list(
        &self,
        list: &ListSpec,
        index: usize,
        vars: &BTreeMap<String, String>,
        ctx: DateContext,
        input: &RunInput<'_>,
        cutoff: Option<DateTime<Utc>>,
        trace: &mut Trace,
    ) -> Result<(Vec<(ListItem, Url)>, Vec<String>)> {
        let label = list.label(index);
        let pages = list.pages();
        let max_pages = pages.map_or(1, |pages| pages.max_pages().clamp(1, MAX_PAGES));
        let html_fields = match list {
            ListSpec::Html(list) => Some(CompiledFields::compile(&list.fields, vars)?),
            _ => None,
        };
        let mut items = Vec::new();
        let mut warnings = Vec::new();
        let mut seen_requests = HashSet::new();
        // 取り直す記事のうち、まだこの一覧で見つけていないもの。
        let mut unseen_rechecks: HashSet<&str> =
            input.recheck_ids.iter().map(String::as_str).collect();
        let mut next: Option<String> = None;
        for page_index in 0..max_pages {
            let first = page_index == 0;
            let request = match self.page_request(list, page_index, next.as_deref(), vars) {
                Ok(request) => request,
                Err(err) if first => return Err(err),
                Err(err) => {
                    warnings.push(format!("{label} の {} ページ目: {err:#}", page_index + 1));
                    break;
                }
            };
            if !seen_requests.insert(request.url.to_string()) {
                break;
            }
            let page_title = if max_pages > 1 {
                format!("{label} {} ページ目", page_index + 1)
            } else {
                label.clone()
            };
            let url = request.url.to_string();
            let fetched = match self.send(input, request).await {
                Ok(fetched) => fetched,
                Err(err) if first => return Err(err),
                Err(err) => {
                    trace.push(Stage::List, page_title, format!("{url}: {err:#}"), false);
                    warnings.push(format!("{label} の {} ページ目: {err:#}", page_index + 1));
                    break;
                }
            };
            let page = match read_page(list, &fetched, first, vars, html_fields.as_ref(), ctx) {
                Ok(page) => page,
                Err(err) if first => return Err(err),
                // 2 ページ目以降を読めなければ、一部の失敗として止める。正常な終わりとみなすと、
                // 新作・近日リリースでは残りのページの記事を一覧から消してしまう。
                Err(err) => {
                    trace.push(Stage::List, page_title, format!("{url}: {err:#}"), false);
                    warnings.push(format!(
                        "{label} の {} ページ目を読めません: {err:#}",
                        page_index + 1
                    ));
                    break;
                }
            };
            let count = page.items.len();
            let raw = page.raw;
            let base = fetched.url.clone();
            // お知らせでは、このページの記事がすべて取得済みか古ければ、先は読まない。
            // 条件で全件を除いたページは、次のページに記事があるかもしれないので続ける。
            // 取り直す記事がまだ見つかっていなければ、取得済みの記事だけのページでも続ける
            // (古い記事だけのページなら、その先にもないので止める)。
            let ids: Vec<Option<String>> = page
                .items
                .iter()
                .map(|item| {
                    let mut resolved = item.clone();
                    resolved.url = item.url.as_deref().and_then(|url| resolve(url, &base));
                    external_id(&resolved)
                })
                .collect();
            for id in ids.iter().flatten() {
                unseen_rechecks.remove(id.as_str());
            }
            let all_old = page
                .items
                .iter()
                .all(|item| is_too_old(item.published, cutoff));
            let nothing_new = !page.items.is_empty()
                && (all_old || unseen_rechecks.is_empty())
                && page.items.iter().zip(&ids).all(|(item, id)| {
                    is_too_old(item.published, cutoff)
                        || id.as_ref().is_some_and(|id| input.known.contains_key(id))
                });
            trace.push(Stage::List, page_title, format!("{url} → {count} 件"), true);
            items.extend(page.items.into_iter().map(|item| (item, base.clone())));
            next = page.next.map(|next| match pages {
                Some(PagesSpec::NextLink(_)) => resolve(&next, &base).unwrap_or(next),
                _ => next,
            });
            let has_next = match pages {
                None => false,
                Some(PagesSpec::Page(_)) => true,
                Some(PagesSpec::NextLink(_) | PagesSpec::Cursor(_)) => next.is_some(),
            };
            if raw == 0 || nothing_new || !has_next {
                break;
            }
        }
        Ok((items, warnings))
    }

    /// 一覧の `page_index` ページ目のリクエスト。`next` は前のページで見つけた
    /// 次のページの URL かカーソル。
    fn page_request(
        &self,
        list: &ListSpec,
        page_index: u32,
        next: Option<&str>,
        vars: &BTreeMap<String, String>,
    ) -> Result<Request> {
        let spec = list.request();
        match list.pages() {
            None => build_request(
                spec,
                &Scope {
                    vars: Some(vars),
                    ..Scope::default()
                },
                None,
            ),
            Some(PagesSpec::Page(numbers)) => {
                let number = (numbers.start + page_index * numbers.step).to_string();
                let scope = Scope {
                    vars: Some(vars),
                    page: Some(&number),
                    ..Scope::default()
                };
                let param = numbers
                    .param
                    .as_deref()
                    .map(|param| (param, number.as_str()));
                build_request(spec, &scope, param)
            }
            Some(PagesSpec::NextLink(_)) => {
                let scope = Scope {
                    vars: Some(vars),
                    ..Scope::default()
                };
                let mut request = build_request(spec, &scope, None)?;
                if page_index > 0 {
                    let next = next.context("次のページの URL がありません")?;
                    request.url = Url::parse(next)
                        .with_context(|| format!("次のページの URL `{next}` を解釈できません"))?;
                    // 次のページは GET で取る。
                    request.method = HttpMethod::Get;
                    request.body = None;
                }
                Ok(request)
            }
            Some(PagesSpec::Cursor(cursor)) => {
                let scope = Scope {
                    vars: Some(vars),
                    ..Scope::default()
                };
                let param = match (page_index, next) {
                    (0, _) => None,
                    (_, Some(next)) => Some((cursor.param.as_str(), next)),
                    (_, None) => bail!("次のページのカーソルがありません"),
                };
                build_request(spec, &scope, param)
            }
        }
    }

    async fn send(&self, input: &RunInput<'_>, request: Request) -> Result<Fetched> {
        if !input.allow_secrets
            && let Some(name) = request
                .headers
                .iter()
                .find(|(_, value)| value.contains("{{secret:"))
                .map(|(name, _)| name)
        {
            bail!("この取得定義では秘密の値を使えません (ヘッダ `{name}`)");
        }
        self.http
            .send(
                input.network,
                request.method,
                request.url.as_str(),
                &request.headers,
                request.body.as_deref(),
            )
            .await
    }

    /// 記事を作る。詳細を取りに行ったかも返す。`revisit` は一覧に見つからなかった取り直す記事
    /// ([`ListItem::from_known`]) で、一覧の署名は保存済みのものを残す。
    #[allow(clippy::too_many_arguments)]
    async fn build_draft(
        &self,
        input: &RunInput<'_>,
        ctx: DateContext,
        external_id: String,
        item: ListItem,
        base: &Url,
        vars: &BTreeMap<String, String>,
        body: &CompiledBody<'_>,
        detail_fields: Option<&CompiledFields>,
        cutoff: Option<DateTime<Utc>>,
        signing: &Signing,
        revisit: bool,
    ) -> Result<(Option<EntryDraft>, bool)> {
        let source = input.source;
        let known = input.known.get(&external_id);
        let signature = match known {
            Some(known) if revisit => known.list_signature.clone(),
            _ => signing.item_signature(&item),
        };

        let detail = match &source.detail {
            Some(detail) => {
                let fresh = known.is_none_or(|known| {
                    needs_work(source, input, &external_id, &item, known, signing)
                });
                if !fresh {
                    // 一覧の値も設定も前回と同じなので、詳細は取り直さず保存済みの値を使う。
                    // 保存済みの値は詳細の値を一覧の値より優先して決めたものなので、
                    // ここで一覧の値に戻してはいけない。ただし画像が一覧の画像と同じ参照
                    // (`url_identity.images`) なら、一覧の今の URL を渡す (保存のときに参照に直り、
                    // 署名の新しい取得 URL が記録される)。
                    let known = known.expect("known");
                    let image_url = match (&known.image_url, &item.image) {
                        (Some(stored), Some(listed))
                            if signing.images.reference(listed).as_ref() == Some(stored) =>
                        {
                            Some(listed.clone())
                        }
                        _ => known.image_url.clone(),
                    };
                    return Ok((
                        Some(EntryDraft {
                            external_id,
                            title: known.title.clone(),
                            url: known.url.clone().or(item.url),
                            published: known.published,
                            body_html: None,
                            image_url,
                            note: known.note.clone(),
                            list_signature: signature,
                        }),
                        false,
                    ));
                }
                let label = item
                    .title
                    .clone()
                    .or_else(|| item.url.clone())
                    .unwrap_or_else(|| external_id.clone());
                Some(
                    self.fetch_detail(detail, detail_fields, &item, &external_id, vars, ctx, input)
                        .await
                        .with_context(|| format!("{label} の詳細"))?,
                )
            }
            None => None,
        };
        let fetched = detail.is_some();
        let detail = detail.unwrap_or_default();

        let published = detail.published.or(item.published);
        if is_too_old(published, cutoff) && !input.recheck_ids.contains(&external_id) {
            return Ok((None, fetched));
        }
        let Some(title) = detail
            .title
            .or(item.title)
            .map(|title| clean_title(&title))
            .filter(|title| !title.is_empty())
        else {
            bail!(
                "{} のタイトルが見つかりません",
                item.url.as_deref().unwrap_or(&external_id)
            );
        };
        let body_base = detail.base.as_ref().unwrap_or(base);
        let image = detail
            .image
            .as_deref()
            .and_then(|image| resolve(&decode_entities(image), body_base))
            .or(item.image);
        let note = detail
            .note
            .or(item.note)
            .map(|note| collapse_whitespace(&note));
        let body_html = body.finish(detail.body.or(item.body), image.as_deref(), body_base);

        Ok((
            Some(EntryDraft {
                external_id,
                title,
                url: item.url,
                published,
                body_html: Some(body_html),
                image_url: image,
                note,
                list_signature: signature,
            }),
            fetched,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    async fn fetch_detail(
        &self,
        detail: &DetailSpec,
        html_fields: Option<&CompiledFields>,
        item: &ListItem,
        external_id: &str,
        vars: &BTreeMap<String, String>,
        ctx: DateContext,
        input: &RunInput<'_>,
    ) -> Result<DetailValues> {
        let values = ItemValues {
            id: item.id.clone().or_else(|| Some(external_id.to_string())),
            url: item.url.clone(),
            title: item.title.clone(),
        };
        let scope = Scope {
            vars: Some(vars),
            item: Some(&values),
            json: item.json.as_ref(),
            ..Scope::default()
        };
        let default_request = RequestSpec::get(DEFAULT_DETAIL_URL);
        let spec = detail.request().unwrap_or(&default_request);
        let request = build_request(spec, &scope, None)?;
        let fetched = self.send(input, request).await?;
        let text = fetched.text(spec.encoding.as_deref());

        let mut values = match detail {
            DetailSpec::Html(_) => {
                let fields = html_fields.context("詳細の項目を解釈できません")?;
                let document = Html::parse_document(&text);
                let root = document.root_element();
                let pick = |value: &Option<CompiledValue>| {
                    value.as_ref().and_then(|value| value.extract(root))
                };
                DetailValues {
                    title: pick(&fields.title),
                    published: pick(&fields.published).and_then(|raw| dates::parse(&raw, ctx)),
                    body: Some(match &fields.body {
                        Some(body) => body.extract(root).unwrap_or_default(),
                        None => body_inner_html(&document),
                    }),
                    image: pick(&fields.image),
                    note: pick(&fields.note),
                    base: None,
                }
            }
            DetailSpec::Json(detail) => {
                let json = parse_json(&text).context("詳細の応答が JSON ではありません")?;
                detail_from_json(read_json_item(&json, &detail.fields, ctx, vars))
            }
            DetailSpec::HtmlJson(detail) => {
                let json = embedded_json(&text, &detail.selector, detail.attr.as_deref())?;
                let target = pointer(&json, &detail.item)
                    .with_context(|| format!("詳細の JSON に `{}` がありません", detail.item))?;
                detail_from_json(read_json_item(target, &detail.fields, ctx, vars))
            }
        };
        values.base = Some(fetched.url);
        Ok(values)
    }
}

/// 一覧の値から署名を作る材料。
struct Signing {
    /// 読み方と整形の設定の指紋。
    fingerprint: String,
    /// 画像の同一性 (`url_identity.images`)。
    images: ImageIdentity,
}

impl Signing {
    fn of(source: &SourceConfig) -> Self {
        Self {
            fingerprint: config_fingerprint(source),
            images: ImageIdentity::of(source),
        }
    }

    /// 一覧の値から決まる署名。設定の指紋も含め、読み方や整形を変えたら取り直す。画像は
    /// 宣言に合えば参照で入れる (署名や期限の問い合わせが変わっても、同じ画像なら同じ署名)。
    fn item_signature(&self, item: &ListItem) -> String {
        let image = item
            .image
            .as_deref()
            .map(|image| self.images.identify(image));
        list_signature(&[
            item.title.as_deref(),
            item.url.as_deref(),
            item.published_raw.as_deref(),
            image.as_deref(),
            item.note.as_deref(),
            Some(&self.fingerprint),
        ])
    }
}

/// 保存済みの記事を、取り直す (または更新する) 必要があるか。
fn needs_work(
    source: &SourceConfig,
    input: &RunInput<'_>,
    external_id: &str,
    item: &ListItem,
    known: &KnownItem,
    signing: &Signing,
) -> bool {
    if known.list_signature != signing.item_signature(item) {
        return true;
    }
    if source.detail.is_none() {
        return false;
    }
    if !known.has_body || input.recheck_ids.contains(external_id) {
        return true;
    }
    let reference = item
        .published
        .or(known.published)
        .map_or(known.first_seen_at, |published| published.at);
    input.now - reference < input.recheck
}

fn detail_from_json(item: ListItem) -> DetailValues {
    DetailValues {
        title: item.title,
        published: item.published,
        body: item.body,
        image: item.image,
        note: item.note,
        base: None,
    }
}

/// 記事を見分ける値。ID → URL → タイトルの順。
fn external_id(item: &ListItem) -> Option<String> {
    item.id
        .clone()
        .or_else(|| item.url.clone())
        .or_else(|| item.title.clone())
}

/// リクエストを組み立てる。`param` があれば問い合わせに足す (同じ名前があれば置き換える)。
fn build_request(
    spec: &RequestSpec,
    scope: &Scope<'_>,
    param: Option<(&str, &str)>,
) -> Result<Request> {
    let rendered = render(&spec.url, scope, Escape::Url)?;
    let mut url =
        Url::parse(&rendered).with_context(|| format!("URL `{rendered}` を解釈できません"))?;
    let mut query: Vec<(String, String)> = Vec::new();
    for (name, value) in &spec.query {
        query.push((name.clone(), render(value, scope, Escape::None)?));
    }
    if let Some((name, value)) = param {
        query.retain(|(existing, _)| existing != name);
        query.push((name.to_string(), value.to_string()));
    }
    if !query.is_empty() {
        let names: Vec<&str> = query.iter().map(|(name, _)| name.as_str()).collect();
        let kept: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(name, _)| !names.contains(&name.as_ref()))
            .map(|(name, value)| (name.into_owned(), value.into_owned()))
            .collect();
        let mut pairs = url.query_pairs_mut();
        pairs.clear();
        for (name, value) in kept.iter().chain(query.iter()) {
            pairs.append_pair(name, value);
        }
        drop(pairs);
    }
    let mut headers = BTreeMap::new();
    for (name, value) in &spec.headers {
        headers.insert(name.clone(), render(value, scope, Escape::None)?);
    }
    let body = spec
        .body
        .as_deref()
        .map(|body| render(body, scope, Escape::None))
        .transpose()?;
    Ok(Request {
        method: spec.method,
        url,
        headers,
        body,
    })
}

/// 事前の取得の値を取り出す。
fn extract_bind(kind: DocumentKind, text: &str, spec: &BindSpec) -> Result<String> {
    let raw = match kind {
        DocumentKind::Json => {
            let json = parse_json(text).context("応答が JSON ではありません")?;
            let path = spec.pointer.as_deref().unwrap_or("");
            pointer(&json, path)
                .and_then(scalar_to_string)
                .with_context(|| format!("値が `{path}` にありません"))?
        }
        DocumentKind::Html => {
            let document = Html::parse_document(text);
            let css = spec.selector.as_deref().unwrap_or("");
            let element = document
                .select(&selector(css)?)
                .next()
                .with_context(|| format!("要素 `{css}` が見つかりません"))?;
            match &spec.attr {
                Some(attr) => element
                    .attr(attr)
                    .map(|value| value.trim().to_string())
                    .with_context(|| format!("要素 `{css}` に属性 `{attr}` がありません"))?,
                None => collapse_whitespace(&element.text().collect::<String>()),
            }
        }
        DocumentKind::Text => text.to_string(),
    };
    let value = match &spec.regex {
        Some(pattern) => Regex::new(pattern)
            .with_context(|| format!("正規表現 `{pattern}` が誤っています"))?
            .captures(&raw)
            .and_then(|captures| captures.get(1))
            .map(|found| found.as_str().trim().to_string())
            .with_context(|| format!("正規表現 `{pattern}` に合う部分がありません"))?,
        None => raw,
    };
    if value.is_empty() {
        bail!("値が空です");
    }
    Ok(value)
}

fn parse_json(text: &str) -> Result<Value> {
    Ok(serde_json::from_str(text.trim_start_matches('\u{feff}'))?)
}

/// HTML の要素に入った JSON を読む。
fn embedded_json(text: &str, css: &str, attr: Option<&str>) -> Result<Value> {
    let element_selector = selector(css)?;
    let document = Html::parse_document(text);
    let element = document
        .select(&element_selector)
        .next()
        .with_context(|| format!("JSON を持つ要素 `{css}` が見つかりません"))?;
    let raw = match attr {
        Some(attr) => element
            .attr(attr)
            .with_context(|| format!("要素 `{css}` に属性 `{attr}` がありません"))?
            .to_string(),
        None => element.text().collect(),
    };
    serde_json::from_str(raw.trim()).context("埋め込まれた値が JSON ではありません")
}

fn is_too_old(published: Option<Published>, cutoff: Option<DateTime<Utc>>) -> bool {
    matches!((published, cutoff), (Some(published), Some(cutoff)) if published.at < cutoff)
}

/// 記事や画像の URL を絶対 URL にする。http と https 以外は捨てる
/// (`ms-settings:` などで手元のアプリを起動されないように)。
fn resolve(raw: &str, base: &Url) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let url = base.join(raw).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.to_string())
}

/// 読み方と整形の設定の指紋。設定を変えたら署名が変わるようにする。
///
/// 画像の同一性 (`url_identity`) は、書いたソースだけ含める (書いていないソースの指紋を、この項目を
/// 足す前と同じにして、詳細を取り直させないため)。決まりを変えたら詳細を取り直し、保存のときに
/// 本文の画像を今の決まりの参照に直す。
fn config_fingerprint(source: &SourceConfig) -> String {
    let base = (
        &source.prepare,
        &source.lists,
        &source.detail,
        &source.body,
        &source.timezone,
    );
    let parts = if source.url_identity == UrlIdentity::default() {
        serde_json::to_string(&base)
    } else {
        serde_json::to_string(&(base, &source.url_identity))
    }
    .unwrap_or_default();
    list_signature(&[Some(&parts)])
}

fn clean_title(title: &str) -> String {
    let text = if title.contains('<') {
        html_to_text(title)
    } else {
        decode_entities(title)
    };
    collapse_whitespace(&text)
}

/// 1 ページを読む。`first` なら一覧の要素が見つからないことを誤りにする。
fn read_page(
    list: &ListSpec,
    fetched: &Fetched,
    first: bool,
    vars: &BTreeMap<String, String>,
    html_fields: Option<&CompiledFields>,
    ctx: DateContext,
) -> Result<Page> {
    let text = || fetched.text(list.request().encoding.as_deref());
    let json_next = |json: &Value| match list.pages() {
        Some(PagesSpec::NextLink(next)) => next
            .pointer
            .as_deref()
            .and_then(|path| pointer(json, path))
            .and_then(scalar_to_string),
        Some(PagesSpec::Cursor(cursor)) => {
            pointer(json, &cursor.pointer).and_then(scalar_to_string)
        }
        _ => None,
    };
    match list {
        ListSpec::Feed(feed) => {
            let items = parse_feed(&fetched.bytes, feed.content)?;
            Ok(Page {
                raw: items.len(),
                items,
                next: None,
            })
        }
        ListSpec::Json(list) => {
            let json = parse_json(&text()).context("応答が JSON ではありません")?;
            let (items, raw) =
                json_items(&json, &list.items, &list.fields, &list.filter, ctx, vars)?;
            Ok(Page {
                items,
                raw,
                next: json_next(&json),
            })
        }
        ListSpec::HtmlJson(list) => {
            let json = embedded_json(&text(), &list.selector, list.attr.as_deref())?;
            let (items, raw) =
                json_items(&json, &list.items, &list.fields, &list.filter, ctx, vars)?;
            Ok(Page {
                items,
                raw,
                next: json_next(&json),
            })
        }
        ListSpec::Html(list) => {
            let fields = html_fields.context("一覧の項目を解釈できません")?;
            let items_selector = selector(&list.items)?;
            let require = list.require.as_deref().map(selector).transpose()?;
            let document = Html::parse_document(&text());
            let elements: Vec<_> = document.select(&items_selector).collect();
            if elements.is_empty() && first {
                bail!(
                    "一覧の要素 `{}` が見つかりません (ページの構造が変わった可能性があります)",
                    list.items
                );
            }
            let next = match &list.pages {
                Some(PagesSpec::NextLink(next)) => next.selector.as_deref().and_then(|css| {
                    let link = document.select(&selector(css).ok()?).next()?;
                    link.attr(next.attr.as_deref().unwrap_or("href"))
                        .map(|href| href.trim().to_string())
                        .filter(|href| !href.is_empty())
                }),
                _ => None,
            };
            let raw = elements.len();
            let items = elements
                .into_iter()
                .filter(|element| {
                    require
                        .as_ref()
                        .is_none_or(|require| element.select(require).next().is_some())
                })
                .map(|element| {
                    let pick = |value: &Option<CompiledValue>| {
                        value.as_ref().and_then(|value| value.extract(element))
                    };
                    let published_raw = pick(&fields.published);
                    ListItem {
                        id: pick(&fields.id),
                        title: pick(&fields.title),
                        url: match &fields.url {
                            Some(url) => url.extract(element),
                            None => default_link(element),
                        },
                        published: published_raw
                            .as_deref()
                            .and_then(|raw| dates::parse(raw, ctx)),
                        published_raw,
                        body: pick(&fields.body),
                        image: pick(&fields.image),
                        note: pick(&fields.note),
                        json: None,
                    }
                })
                .collect();
            Ok(Page { items, raw, next })
        }
    }
}

/// 記事要素のリンク。要素自身が `href` を持たなければ、中の最初のリンク。
fn default_link(element: scraper::ElementRef<'_>) -> Option<String> {
    static LINK: std::sync::LazyLock<Selector> =
        std::sync::LazyLock::new(|| Selector::parse("a[href]").expect("selector"));
    element
        .attr("href")
        .or_else(|| {
            element
                .select(&LINK)
                .next()
                .and_then(|link| link.attr("href"))
        })
        .map(|href| href.trim().to_string())
        .filter(|href| !href.is_empty())
}

fn json_items(
    json: &Value,
    items: &str,
    fields: &JsonFields,
    filter: &[gnn_core::source::Condition],
    ctx: DateContext,
    vars: &BTreeMap<String, String>,
) -> Result<(Vec<ListItem>, usize)> {
    let array = match pointer(json, items) {
        Some(Value::Array(array)) => array,
        Some(_) => bail!(
            "`{}` は配列ではありません",
            if items.is_empty() { "/" } else { items }
        ),
        None => bail!("記事の配列 `{items}` が見つかりません"),
    };
    let items = array
        .iter()
        .filter(|item| filter.iter().all(|condition| matches(item, condition)))
        .map(|item| {
            let mut list_item = read_json_item(item, fields, ctx, vars);
            list_item.json = Some(item.clone());
            list_item
        })
        .collect();
    Ok((items, array.len()))
}

fn read_json_item(
    item: &Value,
    fields: &JsonFields,
    ctx: DateContext,
    vars: &BTreeMap<String, String>,
) -> ListItem {
    let get = |expr: &Option<String>| expr.as_deref().and_then(|expr| eval(item, expr, vars));
    let published_raw = get(&fields.published);
    let published = match fields
        .published
        .as_deref()
        .filter(|expr| !gnn_core::source::is_template(expr))
        .and_then(|expr| pointer(item, expr))
    {
        Some(Value::Number(number)) => number.as_f64().and_then(dates::from_unix_number),
        _ => published_raw
            .as_deref()
            .and_then(|raw| dates::parse(raw, ctx)),
    };
    ListItem {
        id: get(&fields.id),
        title: get(&fields.title),
        url: get(&fields.url),
        published_raw,
        published,
        body: fields
            .body
            .as_ref()
            .and_then(|body| json_body(item, body, vars)),
        image: get(&fields.image),
        note: get(&fields.note),
        json: None,
    }
}

fn json_body(item: &Value, body: &JsonBody, vars: &BTreeMap<String, String>) -> Option<String> {
    match body {
        JsonBody::Value(expr) => eval(item, expr, vars),
        JsonBody::Blocks(blocks) => {
            let Some(Value::Array(array)) = pointer(item, &blocks.blocks) else {
                return None;
            };
            let mut html = String::new();
            for block in array {
                let image = blocks
                    .image
                    .as_deref()
                    .and_then(|path| pointer(block, path))
                    .and_then(scalar_to_string);
                let is_image = match &blocks.image_when {
                    Some(condition) => matches(block, condition),
                    None => image.is_some(),
                };
                if is_image {
                    if let Some(image) = image {
                        html.push_str(&format!("<p><img src=\"{}\"></p>", escape_html(&image)));
                    }
                } else if let Some(text) = blocks
                    .text
                    .as_deref()
                    .and_then(|path| pointer(block, path))
                    .and_then(scalar_to_string)
                {
                    html.push_str(&format!("<div>{text}</div>"));
                }
            }
            (!html.is_empty()).then_some(html)
        }
    }
}

fn parse_feed(bytes: &[u8], content: FeedContent) -> Result<Vec<ListItem>> {
    let feed = feed_rs::parser::parse(bytes).context("フィードを解釈できません")?;
    Ok(feed
        .entries
        .into_iter()
        .map(|entry| {
            let url = entry
                .links
                .iter()
                .find(|link| link.rel.as_deref().is_none_or(|rel| rel == "alternate"))
                .or_else(|| entry.links.first())
                .map(|link| link.href.clone());
            let summary = entry.summary.map(|summary| summary.content);
            let full = entry.content.and_then(|content| content.body);
            let body = match content {
                FeedContent::Auto => full.or(summary),
                FeedContent::Content => full,
                FeedContent::Summary => summary,
            };
            let image = entry
                .media
                .iter()
                .flat_map(|media| {
                    media
                        .thumbnails
                        .iter()
                        .map(|thumbnail| thumbnail.image.uri.clone())
                })
                .next();
            let published = entry.published.or(entry.updated);
            ListItem {
                id: (!entry.id.trim().is_empty()).then(|| entry.id.clone()),
                title: entry.title.map(|title| title.content),
                url,
                published_raw: published.map(|at| at.to_rfc3339()),
                published: published.map(Published::datetime),
                body,
                image,
                note: None,
                json: None,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::HttpOptions;
    use gnn_core::{Secrets, TimePrecision};
    use pretty_assertions::assert_eq;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    type Routes = Arc<parking_lot::Mutex<Vec<(&'static str, &'static str, String)>>>;

    /// パス (問い合わせを含む) ごとに決まった応答を返す HTTP サーバー。
    async fn serve(
        routes: Vec<(&'static str, &'static str, String)>,
    ) -> (String, Arc<parking_lot::Mutex<Vec<String>>>) {
        let (base, hits, _) = serve_changing(routes).await;
        (base, hits)
    }

    /// [`serve`] と同じで、応答をあとから差し替えられる (返す `Routes` を書き換える)。
    async fn serve_changing(
        routes: Vec<(&'static str, &'static str, String)>,
    ) -> (String, Arc<parking_lot::Mutex<Vec<String>>>, Routes) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let hits = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let log = hits.clone();
        let routes: Routes = Arc::new(parking_lot::Mutex::new(routes));
        let table = routes.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let routes = table.clone();
                let log = log.clone();
                tokio::spawn(async move {
                    let mut buffer = vec![0u8; 8192];
                    let read = stream.read(&mut buffer).await.unwrap_or(0);
                    let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                    let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
                    log.lock().push(path.clone());
                    let (status, content_type, body) = routes
                        .lock()
                        .iter()
                        .find(|(route, _, _)| *route == path)
                        .map(|(_, content_type, body)| ("200 OK", *content_type, body.clone()))
                        .unwrap_or(("404 Not Found", "text/plain", "not found".to_string()));
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        (format!("http://{address}"), hits, routes)
    }

    fn engine() -> Engine {
        Engine::new(HttpFetcher::new(HttpOptions::default(), Secrets::default()).unwrap())
    }

    fn now() -> DateTime<Utc> {
        "2026-11-06T03:00:00Z".parse().unwrap()
    }

    /// テスト用の取得定義。`{base}` をサーバーの URL にする。
    fn source(yaml: &str, base: &str) -> SourceConfig {
        let text = format!(
            "schema: 2\nid: test\nname: テスト\n{}",
            yaml.replace("{base}", base)
        );
        let source = SourceConfig::from_yaml(&text).unwrap_or_else(|err| panic!("{err}\n{text}"));
        assert_eq!(source.validate(), vec![], "{text}");
        source
    }

    fn input<'a>(source: &'a SourceConfig, known: &'a HashMap<String, KnownItem>) -> RunInput<'a> {
        RunInput {
            source,
            known,
            max_age_days: 60,
            now: now(),
            recheck: Duration::hours(24),
            recheck_ids: HashSet::new(),
            limit: None,
            network: NetworkPolicy::Any,
            allow_secrets: true,
            trace: true,
        }
    }

    fn known_from(draft: &EntryDraft, first_seen_at: DateTime<Utc>) -> KnownItem {
        KnownItem {
            list_signature: draft.list_signature.clone(),
            first_seen_at,
            has_body: true,
            title: draft.title.clone(),
            url: draft.url.clone(),
            image_url: draft.image_url.clone(),
            note: draft.note.clone(),
            published: draft.published,
        }
    }

    #[tokio::test]
    async fn reads_json_list_with_html_detail_and_skips_known_items() {
        let list = serde_json::json!({
            "articles": [
                {"articleUrl": "1", "title": "メンテナンスのお知らせ", "date": "2026-11-05 10:00", "visible": "True", "bannerImgUrl": "/banner.png"},
                {"articleUrl": "2", "title": "非表示", "date": "2026-11-05 11:00", "visible": "False"},
                {"articleUrl": "3", "title": "古い記事", "date": "2026-01-01 10:00", "visible": "True"}
            ]
        })
        .to_string();
        let (base, hits) = serve(vec![
            ("/list", "application/json", list),
            (
                "/detail?id=1",
                "text/html; charset=utf-8",
                "<html><body><p>11月6日 メンテ<br><br><br>実施</p><script>x</script><div class=\"share\">共有</div></body></html>"
                    .into(),
            ),
        ])
        .await;

        let config = source(
            r#"
lists:
  - kind: json
    request:
      url: '{base}/list'
    items: /articles
    filter:
      - { pointer: /visible, eq: true }
    fields:
      id: /articleUrl
      title: /title
      published: /date
      image: /bannerImgUrl
detail:
  kind: html
  request:
    url: '{base}/detail?id={{item.id}}'
body:
  image_in_body: true
  remove: ['.share']
"#,
            &base,
        );

        let known = HashMap::new();
        let output = engine().run(input(&config, &known)).await;
        assert_eq!(output.status, RunStatus::Ok, "{:?}", output.message);
        assert_eq!(output.listed, 2);
        assert_eq!(output.drafts.len(), 1);
        let draft = &output.drafts[0];
        assert_eq!(draft.title, "メンテナンスのお知らせ");
        assert_eq!(
            draft.published.unwrap().at,
            "2026-11-05T01:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert_eq!(
            draft.image_url.as_deref(),
            Some(format!("{base}/banner.png").as_str())
        );
        assert_eq!(
            draft.body_html.as_deref(),
            Some(
                format!(r#"<p><img src="{base}/banner.png"></p><p>11月6日 メンテ<br><br>実施</p>"#)
                    .as_str()
            )
        );
        let stages: Vec<Stage> = output.trace.iter().map(|step| step.stage).collect();
        assert_eq!(stages, vec![Stage::List, Stage::Merge, Stage::Detail]);

        // 2 回目: 投稿から 1 日以上たっていて一覧の値も同じなら、詳細を取り直さない。
        let known = HashMap::from([(
            "1".to_string(),
            known_from(draft, now() - Duration::minutes(5)),
        )]);
        hits.lock().clear();
        let output = engine().run(input(&config, &known)).await;
        assert_eq!(output.drafts.len(), 1);
        assert_eq!(output.drafts[0].body_html, None);
        assert_eq!(hits.lock().as_slice(), ["/list"]);
    }

    #[tokio::test]
    async fn reads_html_list_and_reports_missing_details() {
        let list = r#"<ul class="news">
            <li><span class="date">2026/11/06</span><a href="/n/1">告知 1</a></li>
            <li><span class="date">2026/11/05</span><a href="/n/2">告知 2</a></li>
            <li><span class="date">2026/11/05</span><a href="/n/1">重複</a></li>
        </ul>"#
            .to_string();
        let (base, _) = serve(vec![
            ("/news", "text/html", list),
            (
                "/n/1",
                "text/html",
                "<div class=\"body\"><p>本文 1</p></div>".into(),
            ),
        ])
        .await;
        let config = source(
            r#"
lists:
  - kind: html
    request:
      url: '{base}/news'
    items: 'ul.news li'
    fields:
      title: a
      published: '.date'
detail:
  kind: html
  fields:
    body: '.body'
"#,
            &base,
        );
        let known = HashMap::new();
        let output = engine().run(input(&config, &known)).await;
        assert_eq!(output.status, RunStatus::Partial);
        assert_eq!(output.listed, 2);
        assert_eq!(output.drafts.len(), 1);
        assert_eq!(output.drafts[0].external_id, format!("{base}/n/1"));
        assert_eq!(
            output.drafts[0].published.unwrap().precision,
            TimePrecision::Date
        );
        assert_eq!(output.drafts[0].body_html.as_deref(), Some("<p>本文 1</p>"));
        assert!(
            output.warnings[0].contains("告知 2"),
            "{:?}",
            output.warnings
        );
    }

    #[tokio::test]
    async fn fails_when_items_are_missing() {
        let (base, _) = serve(vec![("/news", "text/html", "<p>改装中</p>".into())]).await;
        let config = source(
            "lists:\n  - kind: html\n    request:\n      url: '{base}/news'\n    items: 'ul.news li'\n",
            &base,
        );
        let known = HashMap::new();
        let output = engine().run(input(&config, &known)).await;
        assert_eq!(output.status, RunStatus::Failed);
        assert!(output.message.unwrap().contains("ul.news li"));

        let config = source(
            "lists:\n  - kind: feed\n    request:\n      url: '{base}/missing'\n",
            &base,
        );
        let output = engine().run(input(&config, &known)).await;
        assert_eq!(output.status, RunStatus::Failed);
        assert!(output.message.unwrap().contains("404"));
    }

    #[tokio::test]
    async fn prepares_variables_and_reads_feeds() {
        let feed = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0"><channel><title>t</title>
<item><title>新作発表 &amp; 予約開始</title><link>https://example.com/a</link><guid>a</guid>
<pubDate>Thu, 05 Nov 2026 10:00:00 +0900</pubDate><description><![CDATA[<p>概要</p>]]></description></item>
</channel></rss>"#;
        let (base, hits) = serve(vec![
            (
                "/catalog",
                "application/json",
                r#"{"data":{"info":"abc"}}"#.into(),
            ),
            (
                "/page",
                "text/html",
                r#"<html><head><meta name="build" content="b7"></head><script>var token = "t-42";</script></html>"#.into(),
            ),
            ("/feed/abc.xml?build=b7&token=t-42", "application/rss+xml", feed.into()),
        ])
        .await;
        let config = source(
            r#"
prepare:
  - request:
      url: '{base}/catalog'
    bind:
      key: /data/info
  - kind: html
    request:
      url: '{base}/page'
    bind:
      build: { selector: 'meta[name=build]', attr: content }
      token: { selector: script, regex: 'token = "([^"]+)"' }
lists:
  - kind: feed
    request:
      url: '{base}/feed/{{vars.key}}.xml'
      query:
        build: '{{vars.build}}'
        token: '{{vars.token}}'
"#,
            &base,
        );
        let known = HashMap::new();
        let output = engine().run(input(&config, &known)).await;
        assert_eq!(
            output.status,
            RunStatus::Ok,
            "{:?} {:?}",
            output.message,
            hits.lock()
        );
        let draft = &output.drafts[0];
        assert_eq!(draft.title, "新作発表 & 予約開始");
        assert_eq!(draft.url.as_deref(), Some("https://example.com/a"));
        assert_eq!(draft.body_html.as_deref(), Some("<p>概要</p>"));
        assert_eq!(
            draft.published.unwrap().at,
            "2026-11-05T01:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert_eq!(output.trace[0].stage, Stage::Prepare);
        assert!(
            output.trace[1].detail.contains("build = b7"),
            "{:?}",
            output.trace
        );

        // 事前の取得が失敗すれば、取得全体が失敗する。
        let broken = source(
            "prepare:\n  - request:\n      url: '{base}/catalog'\n    bind:\n      key: /nothing\nlists:\n  - kind: feed\n    request:\n      url: '{base}/feed/{{vars.key}}.xml'\n",
            &base,
        );
        let output = engine().run(input(&broken, &known)).await;
        assert_eq!(output.status, RunStatus::Failed);
        assert!(output.message.unwrap().contains("事前の取得"));
    }

    #[tokio::test]
    async fn merges_lists_and_keeps_going_when_one_fails() {
        let news = serde_json::json!({"items": [
            {"id": "n1", "title": "お知らせ 1", "date": "2026-11-05 10:00"},
            {"id": "shared", "title": "お知らせ側", "date": "2026-11-05 09:00"}
        ]})
        .to_string();
        let events = serde_json::json!({"items": [
            {"id": "shared", "title": "イベント側", "date": "2026-11-05 09:00"},
            {"id": "e1", "title": "イベント 1", "date": "2026-11-04 10:00"}
        ]})
        .to_string();
        let (base, _) = serve(vec![
            ("/news", "application/json", news),
            ("/events", "application/json", events),
        ])
        .await;
        let lists = |third: &str| {
            format!(
                r#"
lists:
  - name: お知らせ
    kind: json
    request:
      url: '{{base}}/news'
    items: /items
    fields: {{ id: /id, title: /title, published: /date }}
  - name: イベント
    kind: json
    request:
      url: '{{base}}/events'
    items: /items
    fields: {{ id: /id, title: /title, published: /date }}
{third}"#
            )
        };
        let config = source(&lists(""), &base);
        let known = HashMap::new();
        let output = engine().run(input(&config, &known)).await;
        assert_eq!(output.status, RunStatus::Ok, "{:?}", output.message);
        let titles: Vec<&str> = output
            .drafts
            .iter()
            .map(|draft| draft.title.as_str())
            .collect();
        assert_eq!(titles, vec!["お知らせ 1", "お知らせ側", "イベント 1"]);
        assert_eq!(output.listed_ids, vec!["n1", "shared", "e1"]);

        let failing = "  - name: 壊れた一覧\n    kind: json\n    request:\n      url: '{base}/missing'\n    items: /items\n    fields: { id: /id, title: /title }\n";
        let config = source(&lists(failing), &base);
        let output = engine().run(input(&config, &known)).await;
        assert_eq!(output.status, RunStatus::Partial);
        assert_eq!(output.drafts.len(), 3);
        assert!(
            output.warnings[0].contains("壊れた一覧"),
            "{:?}",
            output.warnings
        );
    }

    #[tokio::test]
    async fn follows_pages_until_nothing_new() {
        let page = |ids: &[&str]| {
            serde_json::json!({
                "items": ids.iter().map(|id| serde_json::json!({"id": id, "title": format!("記事 {id}"), "date": "2026-11-05 10:00"})).collect::<Vec<_>>()
            })
            .to_string()
        };
        let (base, hits) = serve(vec![
            ("/list?page=1", "application/json", page(&["1", "2"])),
            ("/list?page=2", "application/json", page(&["3", "4"])),
            ("/list?page=3", "application/json", page(&[])),
            ("/html/1", "text/html", r#"<ul><li><a href="/a/1">一</a></li></ul><a class="next" href="/html/2">次へ</a>"#.into()),
            ("/html/2", "text/html", r#"<ul><li><a href="/a/2">二</a></li></ul><a class="next" href="/html/1">最初へ</a>"#.into()),
        ])
        .await;
        let config = source(
            r#"
lists:
  - kind: json
    request:
      url: '{base}/list'
    items: /items
    fields: { id: /id, title: /title, published: /date }
    pages: { kind: page, param: page, max_pages: 5 }
"#,
            &base,
        );
        let known = HashMap::new();
        let output = engine().run(input(&config, &known)).await;
        assert_eq!(output.listed_ids, vec!["1", "2", "3", "4"]);
        assert_eq!(
            hits.lock().as_slice(),
            ["/list?page=1", "/list?page=2", "/list?page=3"]
        );

        // 1 ページ目がすべて取得済みなら、2 ページ目は読まない。
        let known: HashMap<String, KnownItem> = output
            .drafts
            .iter()
            .map(|draft| (draft.external_id.clone(), known_from(draft, now())))
            .collect();
        hits.lock().clear();
        engine().run(input(&config, &known)).await;
        assert_eq!(hits.lock().as_slice(), ["/list?page=1"]);

        // 次へのリンクをたどり、同じページに戻ったら止める。
        let config = source(
            r#"
lists:
  - kind: html
    request:
      url: '{base}/html/1'
    items: ul li
    fields:
      title: a
    pages: { kind: next_link, selector: a.next, max_pages: 5 }
"#,
            &base,
        );
        hits.lock().clear();
        let output = engine().run(input(&config, &HashMap::new())).await;
        assert_eq!(output.listed, 2);
        assert_eq!(hits.lock().as_slice(), ["/html/1", "/html/2"]);
    }

    #[tokio::test]
    async fn filtered_pages_do_not_end_paging_and_known_items_do_not_use_the_limit() {
        let page = |items: serde_json::Value| serde_json::json!({ "items": items }).to_string();
        let (base, _) = serve(vec![
            // 1 ページ目は条件で全件を除く。2 ページ目に記事がある。
            (
                "/a?page=1",
                "application/json",
                page(serde_json::json!([
                    {"id": "hidden", "title": "非表示", "visible": false}
                ])),
            ),
            (
                "/a?page=2",
                "application/json",
                page(serde_json::json!([
                    {"id": "k1", "title": "既知 1", "visible": true},
                    {"id": "k2", "title": "既知 2", "visible": true}
                ])),
            ),
            ("/a?page=3", "application/json", page(serde_json::json!([]))),
            (
                "/b",
                "application/json",
                page(serde_json::json!([
                    {"id": "new", "title": "新着", "visible": true}
                ])),
            ),
        ])
        .await;
        let config = source(
            r#"
max_items: 1
lists:
  - kind: json
    request:
      url: '{base}/a'
    items: /items
    filter:
      - { pointer: /visible, eq: true }
    fields: { id: /id, title: /title }
    pages: { kind: page, param: page, max_pages: 5 }
  - kind: json
    request:
      url: '{base}/b'
    items: /items
    fields: { id: /id, title: /title }
"#,
            &base,
        );
        let first = engine()
            .run(RunInput {
                limit: None,
                ..input(&config, &HashMap::new())
            })
            .await;
        assert_eq!(first.listed_ids, vec!["k1", "k2", "new"]);
        // 上限 1 件なので、最初は先頭の 1 件だけを読む。
        assert_eq!(first.drafts.len(), 1);

        // 既知の 2 件が変わっていなければ、上限は新着に使う。
        let mut known = HashMap::new();
        for draft in &first.drafts {
            known.insert(draft.external_id.clone(), known_from(draft, now()));
        }
        let second = engine().run(input(&config, &known)).await;
        let k2_draft = second
            .drafts
            .iter()
            .find(|draft| draft.external_id == "k2")
            .cloned();
        if let Some(draft) = k2_draft {
            known.insert("k2".into(), known_from(&draft, now()));
        }
        let third = engine().run(input(&config, &known)).await;
        let ids: Vec<&str> = third
            .drafts
            .iter()
            .map(|draft| draft.external_id.as_str())
            .collect();
        assert!(ids.contains(&"new"), "{ids:?}");
    }

    #[tokio::test]
    async fn unreadable_later_pages_are_partial_failures() {
        let page = serde_json::json!({"items": [{"id": "1", "title": "告知"}]}).to_string();
        let (base, _) = serve(vec![
            ("/list?page=1", "application/json", page),
            (
                "/list?page=2",
                "text/html",
                "<html>メンテナンス中</html>".into(),
            ),
        ])
        .await;
        let config = source(
            r#"
lists:
  - kind: json
    request:
      url: '{base}/list'
    items: /items
    fields: { id: /id, title: /title }
    pages: { kind: page, param: page, max_pages: 3 }
"#,
            &base,
        );
        let output = engine().run(input(&config, &HashMap::new())).await;
        // 後続ページを読めなかったことを、一部の失敗として返す。
        assert_eq!(output.status, RunStatus::Partial);
        assert!(
            output.warnings[0].contains("2 ページ目"),
            "{:?}",
            output.warnings
        );
        assert_eq!(output.listed_ids, vec!["1"]);
    }

    #[tokio::test]
    async fn reads_embedded_json_details() {
        let list = serde_json::json!({"items": [{"id": "7", "title": "告知"}]}).to_string();
        let detail = r#"<html><script id="data" type="application/json">{"article":{"body":"<p>本文</p>","date":"2026-11-05 12:00"}}</script></html>"#;
        let (base, _) = serve(vec![
            ("/list", "application/json", list),
            ("/news/7", "text/html", detail.into()),
        ])
        .await;
        let config = source(
            r#"
lists:
  - kind: json
    request:
      url: '{base}/list'
    items: /items
    fields: { id: /id, title: /title }
detail:
  kind: html_json
  request:
    url: '{base}/news/{{/id}}'
  selector: '#data'
  item: /article
  fields:
    body: /body
    published: /date
"#,
            &base,
        );
        let output = engine().run(input(&config, &HashMap::new())).await;
        assert_eq!(output.status, RunStatus::Ok, "{:?}", output.message);
        assert_eq!(output.drafts[0].body_html.as_deref(), Some("<p>本文</p>"));
        assert!(output.drafts[0].published.is_some());
    }

    /// 一覧の画像が署名付き (`?s=署名&t=期限`) の JSON の一覧。
    fn signed_list(sig: &str, t: u32) -> String {
        serde_json::json!({"items": [{
            "id": "1",
            "title": "イベント開催",
            "date": "2026-11-03 10:00",
            "image": format!("https://cdn.example.com/secure/img/banner.png?s={sig}&t={t}")
        }]})
        .to_string()
    }

    const SIGNED_SOURCE: &str = r#"
lists:
  - kind: json
    request:
      url: '{base}/list'
    items: /items
    fields: { id: /id, title: /title, published: /date, image: /image }
detail:
  kind: html
  request:
    url: '{base}/detail/{{item.id}}'
  fields:
    body: '.b'
body:
  image_in_body: true
"#;

    const SIGNED_IDENTITY: &str = r#"url_identity:
  images:
    - origin: 'https://cdn.example.com'
      path_prefix: '/secure/'
      ignore_query: ['s', 't']
"#;

    #[tokio::test]
    async fn signed_list_images_do_not_refetch_details() {
        let (base, hits, routes) = serve_changing(vec![
            ("/list", "application/json", signed_list("AAA", 100)),
            (
                "/detail/1",
                "text/html",
                r#"<div class="b">本文<img src="https://cdn.example.com/secure/img/a.png?s=X&amp;t=100"></div>"#
                    .into(),
            ),
        ])
        .await;
        let config = source(&format!("{SIGNED_SOURCE}{SIGNED_IDENTITY}"), &base);
        let identity = ImageIdentity::of(&config);
        let first = engine().run(input(&config, &HashMap::new())).await;
        assert_eq!(first.status, RunStatus::Ok, "{:?}", first.message);
        let draft = first.drafts[0].clone();
        // 下書きは配信元の URL のまま (保存のときに参照にする)。
        let banner = "https://cdn.example.com/secure/img/banner.png?s=AAA&t=100";
        assert_eq!(draft.image_url.as_deref(), Some(banner));
        assert!(
            draft
                .body_html
                .as_deref()
                .unwrap()
                .contains("banner.png?s=AAA&amp;t=100"),
            "{:?}",
            draft.body_html
        );

        // 保存済みの記事 (ストアは画像を参照で持つ)。投稿から 1 日以上たっていて、取り直しの期間の外。
        let mut stored = known_from(&draft, now() - Duration::days(2));
        stored.image_url = identity.reference(banner);
        let known = HashMap::from([("1".to_string(), stored)]);

        // 署名と期限が変わっても、同じ画像なら一覧の署名は同じで、詳細は取り直さない。
        // 画像は一覧の今の URL を渡す (保存のときに同じ参照になり、新しい取得 URL が記録される)。
        routes.lock()[0].2 = signed_list("BBB", 200);
        hits.lock().clear();
        let second = engine().run(input(&config, &known)).await;
        assert_eq!(hits.lock().as_slice(), ["/list"]);
        let skipped = &second.drafts[0];
        assert_eq!(skipped.list_signature, draft.list_signature);
        assert_eq!(skipped.body_html, None);
        assert_eq!(
            skipped.image_url.as_deref(),
            Some("https://cdn.example.com/secure/img/banner.png?s=BBB&t=200")
        );

        // 別の画像になったら、一覧の署名が変わって詳細を取り直す。
        routes.lock()[0].2 = signed_list("CCC", 300).replace("banner.png", "banner2.png");
        hits.lock().clear();
        let third = engine().run(input(&config, &known)).await;
        assert_ne!(third.drafts[0].list_signature, draft.list_signature);
        assert_eq!(hits.lock().as_slice(), ["/list", "/detail/1"]);

        // 宣言がなければ、署名の違いは一覧の値の違い (詳細を取り直す)。
        let plain = source(SIGNED_SOURCE, &base);
        let mut stored = known_from(&draft, now() - Duration::days(2));
        stored.list_signature = engine().run(input(&plain, &HashMap::new())).await.drafts[0]
            .list_signature
            .clone();
        let known = HashMap::from([("1".to_string(), stored)]);
        routes.lock()[0].2 = signed_list("DDD", 400);
        hits.lock().clear();
        engine().run(input(&plain, &known)).await;
        assert_eq!(hits.lock().as_slice(), ["/list", "/detail/1"]);
    }

    #[test]
    fn fingerprint_includes_url_identity_only_when_written() {
        let plain = source(SIGNED_SOURCE, "http://127.0.0.1:1");
        // 書いていないソースの指紋は、この項目を足す前と同じ (アップデートで詳細を取り直させない)。
        let before = list_signature(&[Some(
            &serde_json::to_string(&(
                &plain.prepare,
                &plain.lists,
                &plain.detail,
                &plain.body,
                &plain.timezone,
            ))
            .unwrap(),
        )]);
        assert_eq!(config_fingerprint(&plain), before);
        let declared = source(
            &format!("{SIGNED_SOURCE}{SIGNED_IDENTITY}"),
            "http://127.0.0.1:1",
        );
        assert_ne!(config_fingerprint(&declared), before);
    }

    #[tokio::test]
    async fn skipped_details_keep_stored_values_and_config_changes_refetch() {
        let list = serde_json::json!({"items": [
            {"id": "1", "title": "告知", "date": "2026-11-04 10:00"}
        ]})
        .to_string();
        let (base, hits) = serve(vec![
            ("/list", "application/json", list),
            (
                "/detail/1",
                "text/html",
                "<h1>【追記】告知</h1><div class=\"b\">本文</div>".into(),
            ),
        ])
        .await;
        let yaml = r#"
lists:
  - kind: json
    request:
      url: '{base}/list'
    items: /items
    fields: { id: /id, title: /title, published: /date }
detail:
  kind: html
  request:
    url: '{base}/detail/{{item.id}}'
  fields:
    title: h1
    body: '.b'
"#;
        let mut config = source(yaml, &base);
        let first = engine().run(input(&config, &HashMap::new())).await;
        let draft = first.drafts[0].clone();
        assert_eq!(draft.title, "【追記】告知");

        let known = HashMap::from([(
            "1".to_string(),
            known_from(&draft, now() - Duration::days(2)),
        )]);
        hits.lock().clear();
        let second = engine().run(input(&config, &known)).await;
        // 詳細は取り直さず、詳細から取ったタイトルを一覧のタイトルで上書きしない。
        assert_eq!(second.drafts[0].title, "【追記】告知");
        assert_eq!(second.drafts[0].body_html, None);
        assert_eq!(hits.lock().as_slice(), ["/list"]);

        // 整形の設定を変えたら、保存済みの記事も詳細を取り直す。
        config.body.newlines_to_br = true;
        hits.lock().clear();
        let third = engine().run(input(&config, &known)).await;
        assert!(third.drafts[0].body_html.is_some());
        assert_eq!(hits.lock().as_slice(), ["/list", "/detail/1"]);
    }

    #[tokio::test]
    async fn rechecks_listed_ids_regardless_of_age() {
        let list = serde_json::json!({"items": [
            {"id": "1", "title": "メンテナンスのお知らせ", "date": "2026-10-30 10:00"},
            {"id": "2", "title": "イベント", "date": "2026-10-30 11:00"}
        ]})
        .to_string();
        let (base, hits) = serve(vec![
            ("/list", "application/json", list),
            (
                "/detail/1",
                "text/html",
                "<div class=\"b\">完了しました</div>".into(),
            ),
            (
                "/detail/2",
                "text/html",
                "<div class=\"b\">開催</div>".into(),
            ),
        ])
        .await;
        let config = source(
            r#"
lists:
  - kind: json
    request:
      url: '{base}/list'
    items: /items
    fields: { id: /id, title: /title, published: /date }
detail:
  kind: html
  request:
    url: '{base}/detail/{{item.id}}'
  fields:
    body: '.b'
"#,
            &base,
        );
        let first = engine().run(input(&config, &HashMap::new())).await;
        // 投稿から 1 週間たっていて、一覧の値も変わっていない。
        let known: HashMap<String, KnownItem> = first
            .drafts
            .iter()
            .map(|draft| {
                (
                    draft.external_id.clone(),
                    known_from(draft, now() - Duration::days(7)),
                )
            })
            .collect();
        hits.lock().clear();
        let output = engine()
            .run(RunInput {
                recheck_ids: HashSet::from(["1".to_string()]),
                ..input(&config, &known)
            })
            .await;
        // 指示された記事だけ詳細を取り直す。
        assert_eq!(hits.lock().as_slice(), ["/list", "/detail/1"]);
        let bodies: Vec<(&str, bool)> = output
            .drafts
            .iter()
            .map(|draft| (draft.external_id.as_str(), draft.body_html.is_some()))
            .collect();
        assert_eq!(bodies, vec![("1", true), ("2", false)]);
    }

    #[tokio::test]
    async fn keeps_paging_until_rechecked_items_are_found() {
        let page = |ids: &[&str]| {
            serde_json::json!({
                "items": ids.iter().map(|id| serde_json::json!({"id": id, "title": format!("記事 {id}"), "date": "2026-11-05 10:00"})).collect::<Vec<_>>()
            })
            .to_string()
        };
        let (base, hits) = serve(vec![
            ("/list?page=1", "application/json", page(&["1", "2"])),
            ("/list?page=2", "application/json", page(&["3", "4"])),
            ("/list?page=3", "application/json", page(&["5"])),
        ])
        .await;
        let config = source(
            r#"
lists:
  - kind: json
    request:
      url: '{base}/list'
    items: /items
    fields: { id: /id, title: /title, published: /date }
    pages: { kind: page, param: page, max_pages: 5 }
"#,
            &base,
        );
        let first = engine().run(input(&config, &HashMap::new())).await;
        let known: HashMap<String, KnownItem> = first
            .drafts
            .iter()
            .map(|draft| (draft.external_id.clone(), known_from(draft, now())))
            .collect();
        // 2 ページ目にある記事を取り直すなら、取得済みだけの 1 ページ目で止めずに、見つけたところで止める。
        hits.lock().clear();
        engine()
            .run(RunInput {
                recheck_ids: HashSet::from(["3".to_string()]),
                ..input(&config, &known)
            })
            .await;
        assert_eq!(hits.lock().as_slice(), ["/list?page=1", "/list?page=2"]);
    }

    #[tokio::test]
    async fn rechecks_items_outside_the_list_or_the_period_from_stored_values() {
        // 一覧には新着 1・7 と、取得する期間 (60 日) より古い 2。3・4・5 は一覧から外れた (4 は消えた記事、
        // 5 は配信元に届かない)。6 は保存済みの値もない。上限 (`max_items: 1`) は新着だけで使い切る。
        let list = serde_json::json!({"items": [
            {"id": "1", "url": "/detail/1", "title": "新着", "date": "2026-11-05 10:00"},
            {"id": "2", "url": "/detail/2", "title": "古いお知らせ", "date": "2026-07-01 10:00"},
            {"id": "7", "url": "/detail/7", "title": "もう 1 つの新着", "date": "2026-11-05 11:00"}
        ]})
        .to_string();
        let (base, hits) = serve(vec![
            ("/list", "application/json", list),
            (
                "/detail/1",
                "text/html",
                "<div class=\"b\">新着</div>".into(),
            ),
            (
                "/detail/2",
                "text/html",
                "<div class=\"b\">古い <img src=\"/a.png?s=2\"></div>".into(),
            ),
            (
                "/detail/3",
                "text/html",
                "<div class=\"b\">外れた <img src=\"/b.png?s=2\"></div>".into(),
            ),
        ])
        .await;
        let config = source(
            r#"
lists:
  - kind: json
    request:
      url: '{base}/list'
    items: /items
    fields: { id: /id, url: /url, title: /title, published: /date }
detail:
  kind: html
  fields:
    body: '.b'
max_items: 1
"#,
            &base,
        );
        let stored = |id: &str, title: &str, published: &str| {
            let published = Published::datetime(published.parse().unwrap());
            (
                id.to_string(),
                KnownItem {
                    list_signature: format!("signature-{id}"),
                    first_seen_at: published.at,
                    has_body: true,
                    title: title.into(),
                    url: Some(format!("{base}/detail/{id}")),
                    image_url: Some(format!("{base}/banner-{id}.png")),
                    note: Some("メモ".into()),
                    published: Some(published),
                },
            )
        };
        let mut unreachable = stored("5", "届かない記事", "2026-04-01T01:00:00Z");
        unreachable.1.url = Some("http://127.0.0.1:9/detail/5".into());
        let known: HashMap<String, KnownItem> = [
            stored("1", "新着", "2026-11-05T01:00:00Z"),
            stored("2", "古いお知らせ", "2026-07-01T01:00:00Z"),
            stored("3", "外れた記事", "2026-06-01T01:00:00Z"),
            stored("4", "消えた記事", "2026-05-01T01:00:00Z"),
            unreachable,
        ]
        .into();
        let output = engine()
            .run(RunInput {
                recheck_ids: ["2", "3", "4", "5", "6"]
                    .iter()
                    .map(|id| id.to_string())
                    .collect(),
                ..input(&config, &known)
            })
            .await;
        // 期間より古い 2 も一覧にない 3 も、上限によらず詳細を取り直す。一覧にない記事が取れなくても、
        // ソースの失敗にしない。
        assert_eq!(output.status, RunStatus::Ok, "{:?}", output.warnings);
        let mut paths = hits.lock().clone();
        paths.sort();
        assert_eq!(
            paths,
            ["/detail/1", "/detail/2", "/detail/3", "/detail/4", "/list"]
        );
        // 取り直せなかった理由を返す (消えた・届かない・保存済みの値もない)。
        assert_eq!(
            output.recheck_misses,
            HashMap::from([
                ("4".to_string(), RecheckMiss::Gone),
                ("5".to_string(), RecheckMiss::Failed),
                ("6".to_string(), RecheckMiss::NotFound),
            ])
        );
        // 一覧の件数と ID には、一覧にない記事を入れない (一覧を置き換えるソースで残さないため)。
        assert_eq!(output.listed, 3);
        assert_eq!(output.listed_ids, vec!["1", "2", "7"]);
        let drafts: HashMap<&str, &EntryDraft> = output
            .drafts
            .iter()
            .map(|draft| (draft.external_id.as_str(), draft))
            .collect();
        assert!(
            drafts["2"]
                .body_html
                .as_deref()
                .unwrap()
                .contains("/a.png?s=2")
        );
        // 一覧にない記事は、保存済みの題名・URL・画像・メモ・一覧の署名を使い、本文は詳細から取る。
        let outside = drafts["3"];
        assert_eq!(outside.title, "外れた記事");
        assert_eq!(
            outside.url.as_deref(),
            Some(format!("{base}/detail/3").as_str())
        );
        assert_eq!(
            outside.image_url.as_deref(),
            Some(format!("{base}/banner-3.png").as_str())
        );
        assert_eq!(outside.note.as_deref(), Some("メモ"));
        assert_eq!(outside.list_signature, "signature-3");
        assert_eq!(outside.published, known["3"].published);
        assert!(outside.body_html.as_deref().unwrap().contains("/b.png?s=2"));
        assert!(!drafts.contains_key("4"));
        // 上限を超えた新着 7 は、次の取得で読む。
        assert!(!drafts.contains_key("7"));
        assert!(
            output.trace.iter().any(|step| step.stage == Stage::Detail
                && !step.ok
                && step.title == "一覧にない記事")
        );
    }

    #[tokio::test]
    async fn reports_items_without_identity() {
        let list =
            r#"<ul class="news"><li><span>題なし</span></li><li><a href="/n/1">告知</a></li></ul>"#;
        let (base, _) = serve(vec![
            ("/news", "text/html", list.into()),
            (
                "/empty",
                "text/html",
                r#"<ul class="news"><li></li><li></li></ul>"#.into(),
            ),
        ])
        .await;
        let yaml = |path: &str| {
            format!(
                "lists:\n  - kind: html\n    request:\n      url: '{{base}}/{path}'\n    items: 'ul.news li'\n    fields:\n      title: a\n"
            )
        };
        let known = HashMap::new();
        let config = source(&yaml("news"), &base);
        let output = engine().run(input(&config, &known)).await;
        assert_eq!(output.status, RunStatus::Partial);
        assert_eq!(output.listed_ids, vec![format!("{base}/n/1")]);
        assert!(output.warnings[0].contains("1 件"));

        let config = source(&yaml("empty"), &base);
        let output = engine().run(input(&config, &known)).await;
        assert_eq!(output.status, RunStatus::Failed);
    }

    #[tokio::test]
    async fn public_only_refuses_local_network() {
        let (base, hits) = serve(vec![("/feed", "application/rss+xml", "<rss/>".into())]).await;
        let config = source(
            "lists:\n  - kind: feed\n    request:\n      url: '{base}/feed'\n",
            &base,
        );
        let known = HashMap::new();
        let output = engine()
            .run(RunInput {
                network: NetworkPolicy::PublicOnly,
                ..input(&config, &known)
            })
            .await;
        assert_eq!(output.status, RunStatus::Failed);
        assert!(output.message.unwrap().contains("手元のネットワーク"));
        assert!(hits.lock().is_empty());
    }

    #[test]
    fn keeps_only_web_urls() {
        let base = Url::parse("https://example.com/news/").unwrap();
        assert_eq!(
            resolve("../a", &base).as_deref(),
            Some("https://example.com/a")
        );
        assert_eq!(resolve("ms-settings:display", &base), None);
        assert_eq!(resolve("javascript:alert(1)", &base), None);
        assert_eq!(resolve("file:///C:/Windows/notepad.exe", &base), None);
    }

    #[test]
    fn builds_requests_with_query_and_pages() {
        let vars = BTreeMap::from([("v".to_string(), "a b".to_string())]);
        let spec = RequestSpec {
            url: "https://example.com/list/{{vars.v}}?page=1&keep=1".into(),
            query: BTreeMap::from([("q".to_string(), "x&y={{vars.v}}".to_string())]),
            ..RequestSpec::default()
        };
        let scope = Scope {
            vars: Some(&vars),
            ..Scope::default()
        };
        let request = build_request(&spec, &scope, Some(("page", "3"))).unwrap();
        assert_eq!(
            request.url.as_str(),
            "https://example.com/list/a%20b?keep=1&q=x%26y%3Da+b&page=3"
        );
    }

    #[test]
    fn builds_bodies_from_blocks() {
        let item = serde_json::json!({"bodies": [
            {"type": 2, "text": "前置き", "imageUrl": ""},
            {"type": 6, "text": "", "imageUrl": "https://example.com/a.png"},
            {"type": 4, "text": "本文", "imageUrl": "https://example.com/ignored.png"}
        ]});
        let blocks = JsonBody::Blocks(gnn_core::source::BlocksSpec {
            blocks: "/bodies".into(),
            text: Some("/text".into()),
            image: Some("/imageUrl".into()),
            image_when: Some(gnn_core::source::Condition {
                pointer: "/type".into(),
                eq: Some(serde_json::json!(6)),
                ..Default::default()
            }),
        });
        assert_eq!(
            json_body(&item, &blocks, &BTreeMap::new()).as_deref(),
            Some(r#"<div>前置き</div><p><img src="https://example.com/a.png"></p><div>本文</div>"#)
        );
    }

    #[test]
    fn text_bodies_are_escaped_before_replacement() {
        let spec = BodySpec {
            format: BodyFormat::Text,
            replace: vec![gnn_core::source::ReplaceRule {
                pattern: r"([^\r\n]+\.jpg)[\r\n]+".into(),
                with: r#"<img src="https://example.com/news/$1">"#.into(),
            }],
            ..BodySpec::default()
        };
        let body = CompiledBody::compile(&spec).unwrap();
        let base = Url::parse("https://example.com/").unwrap();
        assert_eq!(
            body.finish(Some("お知らせ<告知>\nbanner.jpg\n本文".into()), None, &base),
            r#"お知らせ&lt;告知&gt;<br><img src="https://example.com/news/banner.jpg">本文"#
        );
    }
}
