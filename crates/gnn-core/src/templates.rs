//! 新しいソースの記入例。取得方式ごとの初期値にする。
//!
//! ゲームごとの取得定義はカタログ (game-news-nexus-config) にあり、アプリには
//! 同梱版を埋め込む (`catalog`)。ここは手元で新しく作るときのひな形だけを持つ。

use crate::source::{
    BodySpec, Channel, HtmlFields, HtmlJsonList, HtmlList, HtmlValue, HtmlValueSpec, JsonBody,
    JsonFields, JsonList, ListKind, ListSpec, SCHEMA_VERSION, ScheduleSpec, SourceConfig,
    UrlIdentity,
};

/// 取得方式ごとの記入例。
pub fn blank(kind: ListKind, id: String) -> SourceConfig {
    let list = match kind {
        ListKind::Feed => ListSpec::empty(kind, "https://example.com/feed.xml"),
        ListKind::Json => {
            let mut list = ListSpec::empty(kind, "https://example.com/api/news.json");
            if let ListSpec::Json(json) = &mut list {
                *json = JsonList {
                    items: "/news".into(),
                    fields: JsonFields {
                        id: Some("/id".into()),
                        title: Some("/title".into()),
                        url: Some("/url".into()),
                        published: Some("/date".into()),
                        body: Some(JsonBody::Value("/body".into())),
                        ..JsonFields::default()
                    },
                    ..json.clone()
                };
            }
            list
        }
        ListKind::Html => {
            let mut list = ListSpec::empty(kind, "https://example.com/news/");
            if let ListSpec::Html(html) = &mut list {
                *html = HtmlList {
                    items: "ul.news li".into(),
                    fields: HtmlFields {
                        title: Some(HtmlValue::Selector(".title".into())),
                        published: Some(HtmlValue::Selector(".date".into())),
                        url: Some(HtmlValue::Spec(HtmlValueSpec {
                            selector: Some("a".into()),
                            attr: Some("href".into()),
                            ..HtmlValueSpec::default()
                        })),
                        ..HtmlFields::default()
                    },
                    ..html.clone()
                };
            }
            list
        }
        ListKind::HtmlJson => {
            let mut list = ListSpec::empty(kind, "https://example.com/news/");
            if let ListSpec::HtmlJson(html_json) = &mut list {
                *html_json = HtmlJsonList {
                    selector: "#news-data".into(),
                    attr: Some("data-items".into()),
                    fields: JsonFields {
                        id: Some("/id".into()),
                        title: Some("/title".into()),
                        published: Some("/date".into()),
                        ..JsonFields::default()
                    },
                    ..html_json.clone()
                };
            }
            list
        }
    };
    SourceConfig {
        schema: SCHEMA_VERSION,
        min_app_version: None,
        id,
        name: "新しいソース".into(),
        channel: Channel::News,
        homepage: None,
        icon: None,
        tags: Vec::new(),
        note: None,
        adult: false,
        general_edition: None,
        inherit_articles: false,
        timezone: None,
        schedule: ScheduleSpec::default(),
        interval_minutes: None,
        max_age_days: None,
        max_items: None,
        prepare: Vec::new(),
        lists: vec![list],
        detail: None,
        body: BodySpec::default(),
        url_identity: UrlIdentity::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_templates_are_valid() {
        for kind in ListKind::ALL {
            let source = blank(kind, "new_source".into());
            assert_eq!(source.kind(), kind);
            assert_eq!(source.validate(), vec![], "{kind:?}");
            let yaml = source.to_yaml().unwrap();
            assert_eq!(SourceConfig::from_yaml(&yaml).unwrap(), source, "{yaml}");
        }
    }
}
