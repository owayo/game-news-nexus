//! 購読 (`subscriptions.toml`)。どのソースを使うか (on/off) と、手元だけの上書き。
//!
//! 取得定義 (YAML) には on/off を持たせない。カタログの定義が更新されても、
//! 利用者の off やアイコンが残るようにするため。

use serde::{Deserialize, Serialize};

/// `subscriptions.toml`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subscriptions {
    #[serde(
        default,
        rename = "subscription",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub items: Vec<Subscription>,
}

impl Subscriptions {
    pub fn get(&self, id: &str) -> Option<&Subscription> {
        self.items.iter().find(|item| item.id == id)
    }

    /// なければ足して返す。足したものは on。
    pub fn entry(&mut self, id: &str) -> &mut Subscription {
        if let Some(index) = self.items.iter().position(|item| item.id == id) {
            return &mut self.items[index];
        }
        self.items.push(Subscription::new(id));
        self.items.last_mut().expect("足した")
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) {
        self.entry(id).enabled = enabled;
    }

    pub fn remove(&mut self, id: &str) {
        self.items.retain(|item| item.id != id);
    }

    /// 手元で作ったアイコンの画像のパス。
    pub fn icons(&self) -> Vec<String> {
        self.items
            .iter()
            .filter_map(|item| item.icon.clone())
            .collect()
    }
}

/// 1 つのソースの購読。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subscription {
    pub id: String,
    /// on/off。off にしても上書きは残す。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 手元で設定したアイコン (切り抜いた画像のパスなど)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// 取得間隔 (分) の上書き。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_minutes: Option<u32>,
    /// これより古い記事を取り込まない日数の上書き。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_age_days: Option<u32>,
    /// 手元のネットワーク (localhost・LAN) への取得を許す。手元の定義だけで効く。
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_private_network: bool,
}

impl Subscription {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            enabled: true,
            icon: None,
            interval_minutes: None,
            max_age_days: None,
            allow_private_network: false,
        }
    }
}

fn default_true() -> bool {
    true
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn roundtrips_and_edits() {
        let mut subscriptions = Subscriptions::default();
        subscriptions.set_enabled("a", false);
        subscriptions.entry("b").icon = Some("C:/icons/b.png".into());
        subscriptions.entry("a").interval_minutes = Some(30);
        let text = toml::to_string_pretty(&subscriptions).unwrap();
        let again: Subscriptions = toml::from_str(&text).unwrap();
        assert_eq!(again, subscriptions);
        assert!(!again.get("a").unwrap().enabled);
        assert!(again.get("b").unwrap().enabled);
        assert_eq!(again.icons(), vec!["C:/icons/b.png".to_string()]);
        subscriptions.remove("a");
        assert!(subscriptions.get("a").is_none());
    }
}
