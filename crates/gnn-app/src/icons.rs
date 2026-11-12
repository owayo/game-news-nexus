//! ゲームのアイコンの切り抜きと保存。
//!
//! 利用者が選んだ画像から正方形を切り抜き、256px の PNG にしてデータの `icons` に置く。
//! ファイル名に保存した時刻を入れ、差し替えたときに古い画像が表示に残らないようにする。

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use image::imageops::FilterType;

/// 保存するアイコンの一辺 (px)。
pub const ICON_SIZE: u32 = 256;

/// 切り抜く正方形。表示上の座標 (画像を縮小・拡大して見せている座標) で持つ。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Selection {
    pub x: f32,
    pub y: f32,
    pub size: f32,
}

impl Selection {
    /// 表示範囲に収まる最大の正方形を中央に置く。
    pub fn centered(view_width: f32, view_height: f32) -> Self {
        let size = view_width.min(view_height);
        Self {
            x: (view_width - size) / 2.0,
            y: (view_height - size) / 2.0,
            size,
        }
    }

    /// 表示範囲からはみ出さないように直す。
    pub fn clamp(self, view_width: f32, view_height: f32, min_size: f32) -> Self {
        let max_size = view_width.min(view_height);
        let size = self.size.clamp(min_size.min(max_size), max_size);
        Self {
            x: self.x.clamp(0.0, view_width - size),
            y: self.y.clamp(0.0, view_height - size),
            size,
        }
    }

    /// 元の画像のピクセル座標 (x, y, 一辺) にする。`scale` は表示 1px あたりの元画像の px。
    pub fn to_source(self, scale: f32, image_width: u32, image_height: u32) -> (u32, u32, u32) {
        let max = image_width.min(image_height).max(1);
        let size = ((self.size * scale).round() as u32).clamp(1, max);
        let x = ((self.x * scale).round() as u32).min(image_width - size);
        let y = ((self.y * scale).round() as u32).min(image_height - size);
        (x, y, size)
    }
}

/// 画像の大きさを読む。読めない形式ならエラー。
pub fn probe(path: &Path) -> Result<(u32, u32)> {
    let (width, height) = image::image_dimensions(path).with_context(|| {
        format!(
            "{} を画像として読めません (PNG・JPEG・GIF・WebP・BMP・ICO に対応)",
            path.display()
        )
    })?;
    if width == 0 || height == 0 {
        anyhow::bail!("画像が空です");
    }
    Ok((width, height))
}

/// 元の画像から正方形を切り抜き、アイコンとして保存する。保存先のパスを返す。
pub fn save_cropped(
    source: &Path,
    (x, y, size): (u32, u32, u32),
    icons_dir: &Path,
    source_id: &str,
) -> Result<PathBuf> {
    let image =
        image::open(source).with_context(|| format!("{} を読めません", source.display()))?;
    let icon =
        image
            .crop_imm(x, y, size, size)
            .resize_exact(ICON_SIZE, ICON_SIZE, FilterType::Lanczos3);
    fs::create_dir_all(icons_dir)
        .with_context(|| format!("{} を作成できません", icons_dir.display()))?;
    let stamp = chrono::Utc::now().timestamp_millis();
    let path = icons_dir.join(format!("{source_id}-{stamp}.png"));
    icon.save_with_format(&path, image::ImageFormat::Png)
        .with_context(|| format!("{} を保存できません", path.display()))?;
    Ok(path)
}

/// 作ったばかりのアイコンは消さない。切り抜いて保存してから購読に書くまでの間に、
/// ほかの変更で片付けが走っても消えないようにする。
const PRUNE_GRACE: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// どのソースからも使われていないアイコンを消す。`icons_dir` の中のファイルだけを対象にし、
/// 作ってから [`PRUNE_GRACE`] たっていないものは残す。
pub fn prune(icons_dir: &Path, in_use: &[String]) -> Result<usize> {
    prune_older_than(icons_dir, in_use, PRUNE_GRACE)
}

fn prune_older_than(
    icons_dir: &Path,
    in_use: &[String],
    grace: std::time::Duration,
) -> Result<usize> {
    let in_use: HashSet<PathBuf> = in_use
        .iter()
        .map(PathBuf::from)
        .filter_map(|path| fs::canonicalize(path).ok())
        .collect();
    let Ok(entries) = fs::read_dir(icons_dir) else {
        return Ok(0);
    };
    let now = std::time::SystemTime::now();
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let recent = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| now.duration_since(modified).unwrap_or_default() < grace);
        if recent {
            continue;
        }
        let canonical = fs::canonicalize(&path).unwrap_or(path.clone());
        if !in_use.contains(&canonical) && fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn selection_starts_centered_and_stays_inside() {
        let selection = Selection::centered(360.0, 200.0);
        assert_eq!(
            selection,
            Selection {
                x: 80.0,
                y: 0.0,
                size: 200.0
            }
        );

        let moved = Selection {
            x: 300.0,
            y: -20.0,
            size: 120.0,
        }
        .clamp(360.0, 200.0, 24.0);
        assert_eq!(
            moved,
            Selection {
                x: 240.0,
                y: 0.0,
                size: 120.0
            }
        );

        let too_big = Selection {
            x: 0.0,
            y: 0.0,
            size: 999.0,
        }
        .clamp(360.0, 200.0, 24.0);
        assert_eq!(too_big.size, 200.0);
        let too_small = Selection {
            x: 10.0,
            y: 10.0,
            size: 2.0,
        }
        .clamp(360.0, 200.0, 24.0);
        assert_eq!(too_small.size, 24.0);
    }

    #[test]
    fn converts_to_source_pixels() {
        // 1000x500 の画像を 360x180 で見せているとき。
        let scale = 1000.0 / 360.0;
        let selection = Selection::centered(360.0, 180.0);
        assert_eq!(selection.to_source(scale, 1000, 500), (250, 0, 500));
        let edge = Selection {
            x: 180.0,
            y: 0.0,
            size: 180.0,
        };
        assert_eq!(edge.to_source(scale, 1000, 500), (500, 0, 500));
    }

    #[test]
    fn crops_saves_and_prunes_icons() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("banner.png");
        let mut banner = image::RgbaImage::new(300, 120);
        for (x, _, pixel) in banner.enumerate_pixels_mut() {
            *pixel = if x < 150 {
                image::Rgba([255, 0, 0, 255])
            } else {
                image::Rgba([0, 0, 255, 255])
            };
        }
        banner.save(&source).unwrap();
        assert_eq!(probe(&source).unwrap(), (300, 120));

        let icons = dir.path().join("icons");
        let saved = save_cropped(&source, (180, 0, 120), &icons, "game").unwrap();
        let icon = image::open(&saved).unwrap().to_rgba8();
        assert_eq!(icon.dimensions(), (ICON_SIZE, ICON_SIZE));
        // 右半分 (青) だけを切り抜いている。
        assert_eq!(icon.get_pixel(128, 128), &image::Rgba([0, 0, 255, 255]));

        let other = save_cropped(&source, (0, 0, 120), &icons, "old").unwrap();
        // 作ったばかりのものは、使っていなくても残す。
        assert_eq!(prune(&icons, &[saved.display().to_string()]).unwrap(), 0);
        assert!(other.exists());
        let in_use = [saved.display().to_string()];
        assert_eq!(
            prune_older_than(&icons, &in_use, std::time::Duration::ZERO).unwrap(),
            1
        );
        assert!(saved.exists());
        assert!(!other.exists());

        assert!(probe(&dir.path().join("missing.png")).is_err());
    }
}
