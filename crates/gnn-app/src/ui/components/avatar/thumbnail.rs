//! ゲームアイコンを画面の物理画素数に合わせて縮小する。

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::{
    App, Asset, Global, ImageCache, ImageSource, ImgResourceLoader, Pixels, RenderImage, Resource,
    Window,
};
use image::{Frame, GenericImageView as _, ImageBuffer, Rgba};

use crate::http_client::{ImageLoader, WatchedImages, display_uri};

pub(super) fn source(icon: &str, size: Pixels) -> ImageSource {
    let resource = if icon.starts_with("http://") || icon.starts_with("https://") {
        Resource::Uri(display_uri(icon).into_owned().into())
    } else {
        Resource::from(PathBuf::from(icon.strip_prefix("file:///").unwrap_or(icon)))
    };
    ImageSource::from(move |window: &mut Window, cx: &mut App| {
        // 通信・デコード失敗時の再取得は本文の画像と同じ経路を使う。
        let original = match cx.try_global::<ImageLoader>() {
            Some(loader) => WatchedImages(loader.0.clone()).load(&resource, window, cx),
            None => window.use_asset::<ImgResourceLoader>(&resource, cx),
        }?;
        let original = match original {
            Ok(image) => image,
            Err(err) => return Some(Err(err)),
        };
        let side = ((f32::from(size) * window.scale_factor()).round() as u32).max(1);
        let bounds = original.size(0);
        if bounds.width.0.min(bounds.height.0) <= side as i32 {
            return Some(Ok(original));
        }
        let request = Thumbnail { original, side };
        for unused in cx.default_global::<RecentThumbnails>().use_image(&request) {
            if let Some(image) = cx.fetch_asset::<Thumbnail>(&unused) {
                cx.drop_image(image, Some(window));
            }
            cx.remove_asset::<Thumbnail>(&unused);
        }
        window.use_asset::<Thumbnail>(&request, cx).map(Ok)
    })
}

#[derive(Clone, PartialEq, Eq)]
struct Thumbnail {
    original: Arc<RenderImage>,
    side: u32,
}

impl Hash for Thumbnail {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // 配信元で画像が変わると、URL が同じでも gpui が新しい ID を割り当てる。
        self.original.id.hash(state);
        self.side.hash(state);
    }
}

impl Asset for Thumbnail {
    type Source = Self;
    type Output = Arc<RenderImage>;

    fn load(request: Self, _: &mut App) -> impl Future<Output = Self::Output> + Send + 'static {
        request.resize()
    }
}

impl Thumbnail {
    async fn resize(self) -> Arc<RenderImage> {
        let mut frames = Vec::with_capacity(self.original.frame_count());
        for i in 0..self.original.frame_count() {
            let size = self.original.size(i);
            let (w, h) = (size.width.0 as u32, size.height.0 as u32);
            let bytes = self.original.as_bytes(i).expect("デコード済みのフレーム");
            let pixels = ImageBuffer::<Rgba<u8>, _>::from_raw(w, h, bytes).expect("BGRA 画像");
            // ObjectFit::Cover と同じく中央の正方形を使う。各色に同じフィルタをかけるので
            // gpui の BGRA の並びのまま補間できる。
            let crop = w.min(h);
            let square = pixels.view((w - crop) / 2, (h - crop) / 2, crop, crop);
            let thumbnail = gnn_icons::resize_icon(&*square, self.side);
            frames.push(Frame::from_parts(thumbnail, 0, 0, self.original.delay(i)));
        }
        Arc::new(RenderImage::new(frames))
    }
}

/// 表示中のアイコンは各画面で共有し、しばらく使わなかったサイズ・差し替え前の画像は GPU からも外す。
#[derive(Default)]
struct RecentThumbnails {
    used: HashMap<Thumbnail, Instant>,
    last_sweep: Option<Instant>,
}

impl Global for RecentThumbnails {}

impl RecentThumbnails {
    fn use_image(&mut self, request: &Thumbnail) -> Vec<Thumbnail> {
        let now = Instant::now();
        let lifetime = Duration::from_secs(60);
        self.used.insert(request.clone(), now);
        if now.duration_since(*self.last_sweep.get_or_insert(now)) < lifetime {
            return Vec::new();
        }
        self.last_sweep = Some(now);
        self.used
            .extract_if(|_, used| now.duration_since(*used) >= lifetime)
            .map(|(key, _)| key)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Delay, RgbaImage};

    #[test]
    fn downsamples_the_center_in_linear_light_and_preserves_animation_and_alpha() {
        // 中央は細い白黒の縞、両端は赤。中央だけを縮小すると、縞が均一な明るさになる。
        let stripes = RgbaImage::from_fn(480, 240, |x, _| match x {
            120..360 if x % 2 == 0 => Rgba([255; 4]),
            120..360 => Rgba([0, 0, 0, 255]),
            _ => Rgba([0, 0, 255, 255]),
        });
        let alpha = RgbaImage::from_fn(480, 240, |x, _| {
            if x % 2 == 0 {
                Rgba([255, 0, 0, 255])
            } else {
                Rgba([0, 255, 0, 0])
            }
        });
        let delay = Delay::from_numer_denom_ms(75, 1);
        let original = Arc::new(RenderImage::new(vec![
            Frame::new(stripes),
            Frame::from_parts(alpha, 0, 0, delay),
        ]));
        // 32 論理 px の等倍表示と 150% 表示。
        for side in [32, 48] {
            let request = Thumbnail {
                original: original.clone(),
                side,
            };
            let image = futures::executor::block_on(request.resize());
            assert_eq!(image.size(0), gpui_kit::size(side.into(), side.into()));
            assert_eq!(image.frame_count(), 2);
            assert_eq!(image.delay(1), delay);
            let middle = ((side / 2 * side + side / 2) * 4) as usize;
            let pixels = image.as_bytes(0).unwrap();
            assert!(
                pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|p| p[0] == p[1] && p[1] == p[2])
            );
            assert!((186..=189).contains(&pixels[middle]));
            let pixel = &image.as_bytes(1).unwrap()[middle..middle + 4];
            assert_eq!(&pixel[..3], &[255, 0, 0]);
            assert!((126..=129).contains(&pixel[3]));
        }
    }
}
