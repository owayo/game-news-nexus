//! 取得のたびに URL が変わる画像を、変わらない参照で持つ。
//!
//! 署名付きの URL (`?s=署名&t=期限`) を配る配信元では、同じ画像の URL が数分から数時間ごとに変わる。
//! URL のまま保存すると、そのたびに記事が「更新」になり、同じ画像を取り直す。取得の定義の
//! `url_identity.images` に合う画像は、宣言した問い合わせを外した URL (比べるための URL) から鍵を作り、
//! 本文と記事の画像には `https://source-image.invalid/<ソース ID>/<鍵>` という参照を入れる。
//! 取りに行くときは、取得のたびに記録する配信元の URL (観測) を使う。比べるための URL は取得には使わない
//! (署名を外すと取れない配信元があるため)。`.invalid` のホストなので、参照のまま外へ取りに行くことはない。

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::Regex;
use sha2::{Digest as _, Sha256};
use url::Url;

use crate::source::{SourceConfig, UrlIdentity};
use crate::text::{decode_entities, escape_html};

/// 参照のホスト。
pub const REFERENCE_HOST: &str = "source-image.invalid";

/// 参照と、そのときの配信元の URL。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImageObservation {
    /// `https://source-image.invalid/<ソース ID>/<鍵>`。
    pub reference: String,
    /// 配信元の URL。署名や期限が付いたまま。
    pub fetch_url: String,
}

/// ソースの `url_identity.images` を、URL に当てはめられる形にしたもの。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageIdentity {
    source_id: String,
    rules: Vec<Rule>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Rule {
    /// `Url::origin` の表記 (`https://cdn.example.com`)。
    origin: String,
    path_prefix: String,
    ignore: Vec<String>,
}

impl ImageIdentity {
    /// 宣言から作る。解釈できない決まり (検査で断られるもの) は使わない。
    pub fn new(source_id: &str, identity: &UrlIdentity) -> Self {
        let rules = identity
            .images
            .iter()
            .filter(|rule| !rule.ignore_query.is_empty())
            .filter_map(|rule| {
                let parsed = Url::parse(&rule.origin).ok()?;
                // 参照 (`.invalid` のホスト) に決まりが合うと、参照を参照に置き換えてしまう。
                if parsed
                    .host_str()
                    .is_none_or(|host| host.ends_with(".invalid"))
                {
                    return None;
                }
                let origin = parsed.origin().ascii_serialization();
                (origin == rule.origin).then(|| Rule {
                    origin,
                    path_prefix: rule.path_prefix().to_string(),
                    ignore: rule.ignore_query.clone(),
                })
            })
            .collect();
        Self {
            source_id: source_id.to_string(),
            rules,
        }
    }

    pub fn of(source: &SourceConfig) -> Self {
        Self::new(&source.id, &source.url_identity)
    }

    /// 決まりが 1 つもないか。
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// 宣言に合う画像の URL なら、比べるための URL (フラグメントと宣言した問い合わせを外したもの)。
    /// ほかの問い合わせは、並びも符号化もそのまま残す。
    pub fn comparable_url(&self, url: &str) -> Option<String> {
        if self.rules.is_empty() {
            return None;
        }
        let mut parsed = Url::parse(url).ok()?;
        let origin = parsed.origin().ascii_serialization();
        let rule = self
            .rules
            .iter()
            .find(|rule| rule.origin == origin && parsed.path().starts_with(&rule.path_prefix))?;
        parsed.set_fragment(None);
        if let Some(query) = parsed.query().map(str::to_string) {
            let kept: Vec<&str> = query
                .split('&')
                .filter(|pair| {
                    let name = url::form_urlencoded::parse(pair.as_bytes())
                        .next()
                        .map(|(name, _)| name);
                    !name.is_some_and(|name| rule.ignore.iter().any(|ignore| *ignore == name))
                })
                .collect();
            parsed.set_query((!kept.is_empty()).then(|| kept.join("&")).as_deref());
        }
        Some(parsed.into())
    }

    /// 宣言に合う画像の URL なら、その参照。
    pub fn reference(&self, url: &str) -> Option<String> {
        let comparable = self.comparable_url(url)?;
        let digest = Sha256::digest(comparable.as_bytes());
        Some(reference_url(&self.source_id, &hex::encode(&digest[..16])))
    }

    /// 宣言に合う画像の URL なら、参照と配信元の URL の組。
    pub fn observe(&self, url: &str) -> Option<ImageObservation> {
        self.reference(url).map(|reference| ImageObservation {
            reference,
            fetch_url: url.to_string(),
        })
    }

    /// 画像の URL を、宣言に合えば参照にする。合わなければそのまま。
    pub fn identify<'a>(&self, url: &'a str) -> Cow<'a, str> {
        match self.reference(url) {
            Some(reference) => Cow::Owned(reference),
            None => Cow::Borrowed(url),
        }
    }

    /// 整形済みの本文の `<img src>` のうち、宣言に合うものを参照に置き換える。置き換えた画像の
    /// 観測 (重なりは除く) も返す。参照になっている `src` はそのまま残す。
    pub fn rewrite_html<'a>(&self, html: &'a str) -> (Cow<'a, str>, Vec<ImageObservation>) {
        static IMG_SRC: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r#"(<img\b[^>]*?\bsrc=")([^"]*)(")"#).expect("regex"));
        if self.rules.is_empty() || !html.contains("<img") {
            return (Cow::Borrowed(html), Vec::new());
        }
        let mut observed: Vec<ImageObservation> = Vec::new();
        let rewritten = IMG_SRC.replace_all(html, |caps: &regex::Captures<'_>| {
            let src = decode_entities(&caps[2]);
            match self.observe(&src) {
                Some(observation) => {
                    let replaced = format!(
                        "{}{}{}",
                        &caps[1],
                        escape_html(&observation.reference),
                        &caps[3]
                    );
                    if !observed.contains(&observation) {
                        observed.push(observation);
                    }
                    replaced
                }
                None => caps[0].to_string(),
            }
        });
        (rewritten, observed)
    }
}

/// (ソース ID, 鍵) の参照。[`parse_reference`] の逆。
pub fn reference_url(source_id: &str, key: &str) -> String {
    format!("https://{REFERENCE_HOST}/{source_id}/{key}")
}

/// 参照を (ソース ID, 鍵) に分ける。参照でなければ `None`。
pub fn parse_reference(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("https://")?.strip_prefix(REFERENCE_HOST)?;
    let (source_id, key) = rest.strip_prefix('/')?.split_once('/')?;
    let valid = !source_id.is_empty()
        && key.len() == 32
        && key
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase());
    valid.then_some((source_id, key))
}

static REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r#"https://{}/[a-z0-9_-]+/[0-9a-f]{{32}}"#,
        regex::escape(REFERENCE_HOST)
    ))
    .expect("regex")
});

/// 本文の参照を、`resolve` (ソース ID, 鍵 → 配信元の URL) が返す URL に戻す。戻せない参照はそのまま。
/// 旧形式への書き出しと、決まりを変えたときの比べ直しに使う。
pub fn expand_references<'a>(
    html: &'a str,
    mut resolve: impl FnMut(&str, &str) -> Option<String>,
) -> Cow<'a, str> {
    if !html.contains(REFERENCE_HOST) {
        return Cow::Borrowed(html);
    }
    REFERENCE.replace_all(html, |caps: &regex::Captures<'_>| {
        let matched = &caps[0];
        match parse_reference(matched).and_then(|(source_id, key)| resolve(source_id, key)) {
            Some(url) => escape_html(&url),
            None => matched.to_string(),
        }
    })
}

/// 本文の参照を (ソース ID, 鍵) ごとに集める。
pub fn references_in(html: &str) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = Vec::new();
    for matched in REFERENCE.find_iter(html) {
        if let Some((source_id, key)) = parse_reference(matched.as_str()) {
            let pair = (source_id.to_string(), key.to_string());
            if !found.contains(&pair) {
                found.push(pair);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::UrlIdentityRule;
    use pretty_assertions::assert_eq;

    fn identity(rules: &[(&str, Option<&str>, &[&str])]) -> ImageIdentity {
        ImageIdentity::new(
            "gc",
            &UrlIdentity {
                images: rules
                    .iter()
                    .map(|(origin, path_prefix, ignore)| UrlIdentityRule {
                        origin: origin.to_string(),
                        path_prefix: path_prefix.map(str::to_string),
                        ignore_query: ignore.iter().map(|name| name.to_string()).collect(),
                    })
                    .collect(),
            },
        )
    }

    fn signed() -> ImageIdentity {
        identity(&[("https://cdn.example.com", Some("/secure/"), &["s", "t"])])
    }

    #[test]
    fn signed_urls_share_one_reference() {
        let identity = signed();
        let first = "https://cdn.example.com/secure/img/a.png?s=AbC-1_x&t=1793955600";
        let second = "https://cdn.example.com/secure/img/a.png?t=1793959200&s=ZyX-9_q";
        assert_eq!(
            identity.comparable_url(first).as_deref(),
            Some("https://cdn.example.com/secure/img/a.png")
        );
        let reference = identity.reference(first).unwrap();
        assert_eq!(
            identity.reference(second).as_deref(),
            Some(reference.as_str())
        );
        // 署名のない URL も同じ画像。
        assert_eq!(
            identity
                .reference("https://cdn.example.com/secure/img/a.png")
                .as_deref(),
            Some(reference.as_str())
        );
        assert!(reference.starts_with("https://source-image.invalid/gc/"));
        assert_eq!(parse_reference(&reference).map(|(id, _)| id), Some("gc"));
        // 別の画像は別の参照。
        assert_ne!(
            identity.reference("https://cdn.example.com/secure/img/b.png?s=AbC-1_x&t=1793955600"),
            Some(reference)
        );
    }

    #[test]
    fn keeps_other_queries_and_ignores_fragments() {
        let identity = signed();
        assert_eq!(
            identity
                .comparable_url("https://cdn.example.com/secure/a.png?w=640&s=x&h=%2C1&t=1#top")
                .as_deref(),
            Some("https://cdn.example.com/secure/a.png?w=640&h=%2C1")
        );
        // 宣言していない問い合わせが違えば別の画像 (版や大きさのことがある)。
        assert_ne!(
            identity.reference("https://cdn.example.com/secure/a.png?v=1&s=x"),
            identity.reference("https://cdn.example.com/secure/a.png?v=2&s=y")
        );
        // 名前は大文字と小文字を区別する。符号化した名前も同じ名前として読む。
        assert_eq!(
            identity
                .comparable_url("https://cdn.example.com/secure/a.png?S=1&%73=2")
                .as_deref(),
            Some("https://cdn.example.com/secure/a.png?S=1")
        );
    }

    #[test]
    fn applies_only_to_the_declared_origin_and_path() {
        let identity = signed();
        for url in [
            "https://cdn.example.com/public/a.png?s=x&t=1",
            "http://cdn.example.com/secure/a.png?s=x&t=1",
            "https://cdn.example.com:8443/secure/a.png?s=x&t=1",
            "https://img.example.com/secure/a.png?s=x&t=1",
            "not a url",
        ] {
            assert_eq!(identity.reference(url), None, "{url}");
            assert_eq!(identity.identify(url), url);
        }
        // 決まりがなければ何もしない。
        let none = ImageIdentity::default();
        assert!(none.is_empty());
        assert_eq!(
            none.reference("https://cdn.example.com/secure/a.png?s=x"),
            None
        );
        // 解釈できない決まりは使わない。
        assert!(identity_with_bad_origin().is_empty());
    }

    fn identity_with_bad_origin() -> ImageIdentity {
        identity(&[
            ("https://cdn.example.com/", None, &["s"]),
            ("cdn.example.com", None, &["s"]),
            ("https://cdn.example.com", None, &[]),
            ("https://source-image.invalid", None, &["s"]),
        ])
    }

    #[test]
    fn rewrites_matching_images_in_cleaned_html() {
        let identity = signed();
        let html = r#"<p><img src="https://cdn.example.com/secure/a.png?s=x&amp;t=1"></p><p>本文<img src="https://other.example.com/b.png?t=1" alt="b"><img width="70%" src="https://cdn.example.com/secure/a.png?s=y&amp;t=2"></p>"#;
        let (rewritten, observed) = identity.rewrite_html(html);
        let reference = identity
            .reference("https://cdn.example.com/secure/a.png")
            .unwrap();
        assert_eq!(
            rewritten,
            format!(
                r#"<p><img src="{reference}"></p><p>本文<img src="https://other.example.com/b.png?t=1" alt="b"><img width="70%" src="{reference}"></p>"#
            )
        );
        assert_eq!(
            observed,
            vec![
                ImageObservation {
                    reference: reference.clone(),
                    fetch_url: "https://cdn.example.com/secure/a.png?s=x&t=1".into(),
                },
                ImageObservation {
                    reference: reference.clone(),
                    fetch_url: "https://cdn.example.com/secure/a.png?s=y&t=2".into(),
                },
            ]
        );
        // 書き換えたものをもう一度書き換えても変わらない。
        let (again, none) = identity.rewrite_html(&rewritten);
        assert_eq!(again, rewritten);
        assert!(none.is_empty());
        assert_eq!(
            references_in(&rewritten),
            vec![("gc".into(), parse_reference(&reference).unwrap().1.into())]
        );
        // 宣言がなければ借りたまま返す。
        assert!(matches!(
            ImageIdentity::default().rewrite_html(html).0,
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn expands_references_back_to_fetch_urls() {
        let identity = signed();
        let fetch_url = "https://cdn.example.com/secure/a.png?s=x&t=1";
        let (rewritten, _) =
            identity.rewrite_html(r#"<p><img src="https://cdn.example.com/secure/a.png?s=x&amp;t=1"><img src="https://cdn.example.com/secure/b.png?s=y"></p>"#);
        let a = identity.reference(fetch_url).unwrap();
        let expanded = expand_references(&rewritten, |source_id, key| {
            assert_eq!(source_id, "gc");
            (Some(key) == parse_reference(&a).map(|(_, key)| key)).then(|| fetch_url.to_string())
        });
        // 戻せる参照は配信元の URL (属性の中なので & は逃がす) に、戻せない参照はそのままにする。
        assert_eq!(
            expanded,
            format!(
                r#"<p><img src="https://cdn.example.com/secure/a.png?s=x&amp;t=1"><img src="{}"></p>"#,
                identity
                    .reference("https://cdn.example.com/secure/b.png")
                    .unwrap()
            )
        );
        // 戻した本文をもう一度書き換えると、同じ参照になる。
        assert_eq!(identity.rewrite_html(&expanded).0, rewritten);
        assert!(matches!(
            expand_references("<p>参照なし</p>", |_, _| None),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn parses_only_well_formed_references() {
        let key = "0123456789abcdef0123456789abcdef";
        assert_eq!(
            parse_reference(&format!("https://source-image.invalid/gc/{key}")),
            Some(("gc", key))
        );
        for bad in [
            format!("http://source-image.invalid/gc/{key}"),
            format!("https://source-image.invalid/{key}"),
            format!("https://source-image.invalid/gc/{}", key.to_uppercase()),
            "https://source-image.invalid/gc/short".to_string(),
            format!("https://legacy-image.invalid/{key}"),
        ] {
            assert_eq!(parse_reference(&bad), None, "{bad}");
        }
    }
}
