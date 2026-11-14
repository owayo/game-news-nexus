//! ソースのアイコンとアプリのロゴ。

use std::sync::{Arc, LazyLock};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::StyledExt as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Hsla, Image, ImageFormat, ImageSource, IntoElement, ObjectFit, ParentElement, Pixels,
    RenderOnce, SharedString, Styled, StyledImage as _, Window, div, img, px, rgb,
};

/// ソースのアイコン。角丸の正方形で、画像がなければ名前の頭文字を出す。
/// サイドバー・記事の一覧・本文・ソース画面で同じものを使う。
#[derive(IntoElement)]
pub struct SourceAvatar {
    name: SharedString,
    icon: Option<SharedString>,
    size: Pixels,
}

impl SourceAvatar {
    pub fn new(name: impl Into<SharedString>, icon: Option<impl Into<SharedString>>) -> Self {
        Self {
            name: name.into(),
            icon: icon
                .map(Into::into)
                .filter(|icon: &SharedString| !icon.trim().is_empty()),
            size: px(28.),
        }
    }

    /// 記事の一覧の行の見出しに置く大きさ (16px)。
    pub fn xsmall(mut self) -> Self {
        self.size = px(16.);
        self
    }

    /// サイドバーと本文の見出しに置く大きさ (20px)。
    pub fn small(mut self) -> Self {
        self.size = px(20.);
        self
    }

    /// ソース画面の見出しに置く大きさ (40px)。
    pub fn large(mut self) -> Self {
        self.size = px(40.);
        self
    }
}

impl RenderOnce for SourceAvatar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let radius = (self.size * 0.24).max(px(4.));
        let tile = div()
            .size(self.size)
            .flex_shrink_0()
            .rounded(radius)
            .overflow_hidden();
        match self.icon {
            // gpui は親の角丸で子を切り抜かないので、画像そのものを角丸で描く。
            Some(icon) => tile.bg(cx.theme().muted).child(
                img(icon_source(icon.trim()))
                    .size_full()
                    .rounded(radius)
                    .object_fit(ObjectFit::Cover),
            ),
            None => tile
                .bg(name_color(&self.name))
                .flex()
                .items_center()
                .justify_center()
                .text_color(gpui_kit::white())
                .text_size(self.size * 0.5)
                .line_height(self.size)
                .font_semibold()
                .when(self.size >= px(20.), |this| this.font_bold())
                .child(initial(&self.name)),
        }
    }
}

/// 名前の頭文字。英字は大文字にする。
fn initial(name: &str) -> String {
    name.trim()
        .chars()
        .next()
        .map(|ch| ch.to_uppercase().collect())
        .unwrap_or_default()
}

/// 頭文字のアイコンの背景色。白い文字とのコントラスト比が 4.5:1 を超える色から、名前で選ぶ
/// (同じ名前なら同じ色)。
const AVATAR_COLORS: [u32; 12] = [
    0xBE123C, 0xB45309, 0x4D7C0F, 0x047857, 0x0E7490, 0x1D4ED8, 0x4338CA, 0x6D28D9, 0xA21CAF,
    0x9F1239, 0x475569, 0x7C2D12,
];

fn name_color(name: &str) -> Hsla {
    let hash = name.bytes().fold(0u32, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(byte.into())
    });
    rgb(AVATAR_COLORS[hash as usize % AVATAR_COLORS.len()]).into()
}

/// アイコンの指定を画像の読み込み元にする。URL はそのまま、それ以外は手元のファイル。
fn icon_source(icon: &str) -> ImageSource {
    if icon.starts_with("http://") || icon.starts_with("https://") {
        ImageSource::from(crate::http_client::display_uri(icon).into_owned())
    } else {
        let path = icon.strip_prefix("file:///").unwrap_or(icon);
        ImageSource::from(std::path::PathBuf::from(path))
    }
}

/// アプリのロゴ (アプリのアイコンと同じ絵)。タイトルバー・本文の欄の案内・設定に置く。
#[derive(IntoElement)]
pub struct AppLogo {
    size: Pixels,
}

/// 書き出したロゴの一辺 (px) と画像。小さい順。
///
/// gpui は画像をミップマップなしの線形補間で縮めるので、2 倍を超えて縮めると粗くなる。
/// 出す大きさ (20・40・72px) の 2 倍で書き出してあり (`mise run icons`)、画面の倍率に合うものを選ぶ。
static LOGOS: LazyLock<[(f32, Arc<Image>); 3]> = LazyLock::new(|| {
    let png = |bytes: &[u8]| Arc::new(Image::from_bytes(ImageFormat::Png, bytes.to_vec()));
    [
        (40., png(include_bytes!("../../../assets/icons/app-40.png"))),
        (80., png(include_bytes!("../../../assets/icons/app-80.png"))),
        (
            144.,
            png(include_bytes!("../../../assets/icons/app-144.png")),
        ),
    ]
});

impl AppLogo {
    pub fn new(size: Pixels) -> Self {
        Self { size }
    }
}

/// 画面の画素で `needed` px 以上あるロゴのうち、いちばん小さいもの (なければいちばん大きいもの)。
fn logo_for(needed: f32) -> &'static Arc<Image> {
    let logos = &*LOGOS;
    let (_, image) = logos
        .iter()
        .find(|(side, _)| *side >= needed)
        .unwrap_or(&logos[logos.len() - 1]);
    image
}

impl RenderOnce for AppLogo {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        let needed = f32::from(self.size) * window.scale_factor();
        img(logo_for(needed).clone())
            .size(self.size)
            .flex_shrink_0()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avatar_colors_are_stable_and_readable() {
        assert_eq!(name_color("原神"), name_color("原神"));
        assert_eq!(initial(" twinkle"), "T");
        assert_eq!(initial(""), "");
        // 白い文字とのコントラスト比 (WCAG) が 4.5:1 を超える。
        let luminance = |hex: u32| {
            let channel = |shift: u32| {
                let value = ((hex >> shift) & 0xFF) as f64 / 255.0;
                if value <= 0.03928 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
        };
        for color in AVATAR_COLORS {
            let ratio = 1.05 / (luminance(color) + 0.05);
            assert!(ratio > 4.5, "{color:06X} は {ratio:.2}");
        }
    }

    #[test]
    fn logo_is_never_shrunk_more_than_twice() {
        // 埋め込んだ PNG が書き出した一辺のとおりか。
        for (side, image) in LOGOS.iter() {
            let bytes = image.bytes();
            let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
            let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
            assert_eq!((width as f32, height as f32), (*side, *side));
        }
        let side = |needed: f32| {
            LOGOS
                .iter()
                .find(|(_, image)| Arc::ptr_eq(image, logo_for(needed)))
                .map(|(side, _)| *side)
                .unwrap()
        };
        // 画面に置く大きさ (20・40・72px) を、よく使う倍率 (100〜250%) で出すとき。
        for size in [20., 40., 72.] {
            for scale in [1.0, 1.25, 1.5, 1.75, 2.0, 2.25, 2.5] {
                let needed: f32 = size * scale;
                let chosen = side(needed);
                assert!(
                    chosen / needed <= 2.0,
                    "{size}px を {scale} 倍で出すのに {chosen}px の画像を縮める"
                );
                // いちばん大きい画像を超えないかぎり、足りない画像は選ばない (引き伸ばさない)。
                assert!(chosen >= needed.min(144.));
            }
        }
    }
}
