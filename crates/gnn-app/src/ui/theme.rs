//! アプリの配色。
//!
//! テーマ (`assets/theme.json`) は gpui-kit の ThemeConfig の書式で、主色はアイコンのローズ
//! (#E64969) から作る (ライトは白い文字が読める濃さ、ダークは暗い文字を合わせる明るさ)。
//! 未指定の色の一部は gpui-kit の既定のテーマに戻り、一部は primary などから導かれるので、
//! 画面で使う色は明示する。一覧の選択 (`list.active`) と文字の選択 (`selection`) は gpui-kit が
//! 透明度を 20%・30% までに丸めるので、半透明で書く。
//!
//! テーマにない色 (カードの面・選んだナビ・予定の色) は [`Palette`] が持つ。

use std::rc::Rc;

use gnn_core::ThemePreference;
use gnn_core::agenda::Tone;
use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode, ThemeRegistry, ThemeSet};
use gpui_kit::{App, Hsla, rgb};

const THEMES: &str = include_str!("../../assets/theme.json");

/// gpui-kit のテーマを、このアプリのライトとダークに差し替える。`gpui_kit::init` の後に 1 回呼び、
/// そのあと [`apply`] で利用者の選んだ外観にする。
pub fn install(cx: &mut App) {
    let set: ThemeSet = serde_json::from_str(THEMES).expect("assets/theme.json は ThemeSet の書式");
    // コードの色分け (YAML の表示) は gpui-kit の既定のテーマのものを使う。
    let registry = ThemeRegistry::global(cx);
    let light_highlight = registry.default_light_theme().highlight.clone();
    let dark_highlight = registry.default_dark_theme().highlight.clone();
    Theme::update(cx, move |theme| {
        for mut config in set.themes {
            if config.mode.is_dark() {
                config.highlight = dark_highlight.clone();
                theme.dark_theme = Rc::new(config);
            } else {
                config.highlight = light_highlight.clone();
                theme.light_theme = Rc::new(config);
            }
        }
    });
}

/// テーマを設定に合わせる。
pub fn apply(preference: ThemePreference, cx: &mut App) {
    match preference {
        ThemePreference::System => Theme::sync_system_appearance(None, cx),
        ThemePreference::Light => Theme::change(ThemeMode::Light, None, cx),
        ThemePreference::Dark => Theme::change(ThemeMode::Dark, None, cx),
    }
}

/// テーマにない色。ライトとダークで値が違う。
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    dark: bool,
    /// カード・予定の欄など、地の上に置く面。
    pub surface: Hsla,
    /// 一覧の行どうしの区切り (枠より淡い)。
    pub divider: Hsla,
    /// 選んだナビの項目・選んだ件数の背景。
    pub selected: Hsla,
    /// 選んだナビの項目・選んだ件数の文字。
    pub selected_foreground: Hsla,
}

impl Palette {
    pub fn of(cx: &App) -> Self {
        Self::for_mode(cx.theme().is_dark())
    }

    pub fn for_mode(dark: bool) -> Self {
        let color = |hex: u32| -> Hsla { rgb(hex).into() };
        if dark {
            Self {
                dark,
                surface: color(0x201C25),
                divider: color(0x2C2633),
                selected: color(0x432837),
                selected_foreground: color(0xFFB1C2),
            }
        } else {
            Self {
                dark,
                surface: color(0xFFFFFF),
                divider: color(0xECE6EE),
                selected: color(0xFCE6EE),
                selected_foreground: color(0x922445),
            }
        }
    }

    /// 予定の色合いの文字色と背景色。開始・これからは青、終わり間近・要確認は橙、メンテ中は赤、
    /// 完了は緑。どれも文字と背景のコントラスト比は 4.5:1 を超える。
    pub fn tone(&self, tone: Tone) -> (Hsla, Hsla) {
        let pair = |foreground: u32, background: u32| -> (Hsla, Hsla) {
            (rgb(foreground).into(), rgb(background).into())
        };
        match (tone, self.dark) {
            (Tone::Scheduled | Tone::Starting, false) => pair(0x244DA5, 0xEAF1FF),
            (Tone::Scheduled | Tone::Starting, true) => pair(0xBDD2FF, 0x233451),
            (Tone::Ending | Tone::Attention, false) => pair(0x935007, 0xFFF0DE),
            (Tone::Ending | Tone::Attention, true) => pair(0xFFD29B, 0x422F1F),
            (Tone::Active, false) => pair(0xAA2C2B, 0xFDEDEC),
            (Tone::Active, true) => pair(0xFFB6AC, 0x452625),
            (Tone::Done, false) => pair(0x24623F, 0xE8F4EC),
            (Tone::Done, true) => pair(0xABE0BF, 0x223D30),
            (Tone::Muted, false) => pair(0x6D6573, 0xF1EDF2),
            (Tone::Muted, true) => pair(0xB8ADBE, 0x2B2531),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::component::ThemeConfigColors;

    #[test]
    fn theme_file_has_light_and_dark() {
        let set: ThemeSet = serde_json::from_str(THEMES).unwrap();
        let modes: Vec<bool> = set
            .themes
            .iter()
            .map(|config| config.mode.is_dark())
            .collect();
        assert_eq!(modes, vec![false, true]);
        for config in &set.themes {
            // 画面で使う色は既定のテーマに任せず、明示する。
            let colors = serde_json::to_value(&config.colors).unwrap();
            for key in [
                "background",
                "foreground",
                "primary.background",
                "primary.foreground",
                "ring",
                "border",
                "input.border",
                "muted.background",
                "muted.foreground",
                "list.active.background",
                "list.hover.background",
                "selection.background",
                "sidebar.background",
                "title_bar.background",
                "status_bar.background",
                "popover.background",
                "link",
            ] {
                assert!(
                    colors.get(key).is_some_and(|value| !value.is_null()),
                    "{} に {key} がない",
                    config.name
                );
            }
        }
    }

    /// gpui-kit は知らない色の名前を黙って読み飛ばすので、書いた名前がすべて使われることを確かめる。
    #[test]
    fn theme_file_uses_only_known_color_names() {
        let known: Vec<String> = serde_json::to_value(ThemeConfigColors::default())
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        let raw: serde_json::Value = serde_json::from_str(THEMES).unwrap();
        for theme in raw["themes"].as_array().unwrap() {
            for key in theme["colors"].as_object().unwrap().keys() {
                assert!(
                    known.contains(key),
                    "{} の {key} は gpui-kit のテーマにない色の名前",
                    theme["name"]
                );
            }
        }
    }
}
