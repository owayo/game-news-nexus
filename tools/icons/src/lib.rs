//! 配布アイコンと画面内のロゴで使う拡大・縮小。

use image::imageops::{FilterType, resize};
use image::{Rgba, Rgba32FImage, RgbaImage};

/// 原画の色と透明度をリニア RGB の乗算済みアルファで補間し、sRGB の画像に戻す。
pub fn resize_icon(image: &RgbaImage, side: u32) -> RgbaImage {
    if image.dimensions() == (side, side) {
        return image.clone();
    }

    let linear = Rgba32FImage::from_fn(image.width(), image.height(), |x, y| {
        let [red, green, blue, alpha] = image.get_pixel(x, y).0;
        let alpha = f32::from(alpha) / 255.0;
        Rgba([
            srgb_to_linear(red) * alpha,
            srgb_to_linear(green) * alpha,
            srgb_to_linear(blue) * alpha,
            alpha,
        ])
    });
    let resized = resize(&linear, side, side, FilterType::Lanczos3);
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
