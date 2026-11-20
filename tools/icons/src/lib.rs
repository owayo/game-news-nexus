//! 配布アイコンと画面内のロゴで使う拡大・縮小。

use image::imageops::{FilterType, resize};
use image::{GenericImageView, Rgba, RgbaImage};
use std::sync::LazyLock;

/// 原画の色と透明度をリニア RGB の乗算済みアルファで補間し、sRGB の画像に戻す。
pub fn resize_icon(image: &impl GenericImageView<Pixel = Rgba<u8>>, side: u32) -> RgbaImage {
    if image.dimensions() == (side, side) {
        return RgbaImage::from_fn(side, side, |x, y| image.get_pixel(x, y));
    }

    let resized = resize(&LinearPixels(image), side, side, FilterType::Lanczos3);
    RgbaImage::from_fn(side, side, |x, y| {
        let [red, green, blue, alpha] = resized.get_pixel(x, y).0;
        let opacity = (alpha * 255.0).round() as u8;
        if opacity == 0 {
            return Rgba([0; 4]);
        }
        Rgba([
            linear_to_srgb(red / alpha),
            linear_to_srgb(green / alpha),
            linear_to_srgb(blue / alpha),
            opacity,
        ])
    })
}

/// 縮小に使う画素だけを変換し、大きな原画と同じ寸法の浮動小数点バッファを作らない。
struct LinearPixels<'a, I>(&'a I);

impl<I: GenericImageView<Pixel = Rgba<u8>>> GenericImageView for LinearPixels<'_, I> {
    type Pixel = Rgba<f32>;

    fn dimensions(&self) -> (u32, u32) {
        self.0.dimensions()
    }

    fn get_pixel(&self, x: u32, y: u32) -> Self::Pixel {
        static LINEAR: LazyLock<[f32; 256]> =
            LazyLock::new(|| std::array::from_fn(|value| srgb_to_linear(value as u8)));
        let [r, g, b, a] = self.0.get_pixel(x, y).0;
        let a = f32::from(a) / 255.0;
        Rgba([
            LINEAR[r as usize] * a,
            LINEAR[g as usize] * a,
            LINEAR[b as usize] * a,
            a,
        ])
    }
}

fn srgb_to_linear(value: u8) -> f32 {
    let value = f32::from(value) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(value: f32) -> u8 {
    let value = value.clamp(0.0, 1.0);
    let srgb = if value <= 0.003_130_8 {
        12.92 * value
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (srgb * 255.0).round() as u8
}
