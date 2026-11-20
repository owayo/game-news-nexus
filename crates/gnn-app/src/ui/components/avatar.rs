//! ソースのアイコンとアプリのロゴ。

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::StyledExt as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Hsla, IntoElement, ObjectFit, ParentElement, Pixels, RenderImage, RenderOnce,
    SharedString, Styled, StyledImage as _, Window, div, img, px, rgb,
};
use image::RgbaImage;
use parking_lot::Mutex;

mod thumbnail;

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
            size: px(32.),
        }
    }

    /// お知らせと予定の行でゲーム名に添える大きさ。
    pub fn xsmall(mut self) -> Self {
        self.size = px(20.);
        self
    }

    /// ゲーム選択メニューと本文の見出しに置く大きさ。
    pub fn small(mut self) -> Self {
        self.size = px(24.);
        self
    }

    /// ピックアップやソース画面でゲームのアイコンを大きく見せるときに使う。
    pub fn large(mut self) -> Self {
        self.size = px(40.);
        self
    }
}

impl RenderOnce for SourceAvatar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let radius = (self.size * 0.24).max(px(4.));
        let size = self.size;
        let tile = move || {
            div()
                .size(size)
                .flex_shrink_0()
                .rounded(radius)
                .overflow_hidden()
        };
        let initials = move || {
            tile()
                .bg(name_color(&self.name))
                .flex()
                .items_center()
                .justify_center()
                .text_color(gpui_kit::white())
                .text_size(size * 0.5)
                .line_height(size)
                .font_semibold()
                .when(size >= px(20.), |this| this.font_bold())
                .child(initial(&self.name))
        };
        match self.icon {
            // gpui は親の角丸で子を切り抜かないので、画像そのものを角丸で描く。
            Some(icon) => tile().bg(cx.theme().muted).child(
                img(thumbnail::source(icon.trim(), size))
                    .size_full()
                    .rounded(radius)
                    .with_fallback(move || initials().into_any_element())
                    .object_fit(ObjectFit::Cover),
            ),
            None => initials(),
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

/// アプリのロゴ (アプリのアイコンと同じ絵)。タイトルバー・本文の欄の案内・設定に置く。
#[derive(IntoElement)]
pub struct AppLogo {
    size: Pixels,
}

static LOGO: LazyLock<RgbaImage> = LazyLock::new(|| {
    image::load_from_memory_with_format(
        include_bytes!("../../../assets/icons/app.png"),
        image::ImageFormat::Png,
    )
    .expect("同梱のロゴは PNG")
    .into_rgba8()
});

static LOGOS: LazyLock<Mutex<HashMap<u32, Arc<RenderImage>>>> = LazyLock::new(Mutex::default);

impl AppLogo {
    pub fn new(size: Pixels) -> Self {
        Self { size }
    }
}

/// 画面の画素数に合わせたロゴを作り、同じ大きさの表示で使い回す。
fn logo_for(side: u32) -> Arc<RenderImage> {
    LOGOS
        .lock()
        .entry(side)
        .or_insert_with(|| {
            let mut pixels = gnn_icons::resize_icon(&*LOGO, side);
            // gpui の RenderImage は BGRA。
            for pixel in pixels.pixels_mut() {
                pixel.0.swap(0, 2);
            }
            Arc::new(RenderImage::new(vec![image::Frame::new(pixels)]))
        })
        .clone()
}

impl RenderOnce for AppLogo {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        let side = (f32::from(self.size) * window.scale_factor()).round() as u32;
        img(logo_for(side.max(1))).size(self.size).flex_shrink_0()
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
    fn logo_uses_requested_pixels_and_preserves_alpha() {
        for side in [20, 25, 40, 60, 72, 90, 180] {
            let image = logo_for(side);
            assert_eq!(image.size(0), gpui_kit::size(side.into(), side.into()));
            assert!(Arc::ptr_eq(&image, &logo_for(side)));
            let pixels = image.as_bytes(0).unwrap();
            // Lanczos の補間で、ごく薄い半透明の画素が端に出ることがある。
            assert!(pixels[3] <= 16);
            let center = (side as usize / 2) * (side as usize + 1);
            assert_eq!(pixels[center * 4 + 3], 255);
            assert!(
                pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| (1..255).contains(&pixel[3]))
            );
        }
    }
}
