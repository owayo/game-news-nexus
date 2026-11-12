//! アプリアイコンを SVG から書き出す。`mise run icons` で動かす。
//!
//! 元の絵は `crates/gnn-app/assets/icons/` の 3 つ。
//! - `app.svg`: アプリのアイコン。全面に描く (Windows は全面のまま使う)。
//! - `tray.svg`: 小さい表示に合わせた Windows のトレイのアイコン。
//! - `tray-template.svg`: macOS のメニューバーのアイコン。黒と透明だけで描く (テンプレート画像)。
//!
//! 書き出すもの (同じディレクトリ):
//! - `app.ico`: Windows の実行ファイルとウィンドウのアイコン (16〜256px)
//! - `app.icns`: macOS のアプリのアイコン (16〜1024px)。Apple のアイコンの格子に合わせて、
//!   1024px の中に 824px で置き、影を付ける
//! - `app.png`: 512px (README などに使う)
//! - `tray.png`: Windows のタスクトレイ (64px)
//! - `tray-template.png`: macOS のメニューバー (36px。18pt の 2 倍)
//! - `docs/images/app.png`: README のロゴ (256px)
//!
//! `gnn-icons preview <SVG> <PNG>` は、絵をいくつかの大きさで並べた確認用の画像を作る
//! (明るい背景と暗い背景の 2 段)。

use std::fs;
use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use image::codecs::ico::{IcoEncoder, IcoFrame};
use image::{ExtendedColorType, ImageEncoder as _, Rgba, RgbaImage};
use resvg::{tiny_skia, usvg};

/// Windows のアイコンに入れる大きさ。表示の倍率 (100〜250%) で使われる大きさをそろえる。
const ICO_SIZES: [u32; 10] = [16, 20, 24, 30, 32, 40, 48, 64, 128, 256];

/// macOS のアイコンに入れる大きさと種類。
const ICNS_TYPES: [(u32, icns::IconType); 10] = [
    (16, icns::IconType::RGBA32_16x16),
    (32, icns::IconType::RGBA32_16x16_2x),
    (32, icns::IconType::RGBA32_32x32),
    (64, icns::IconType::RGBA32_32x32_2x),
    (128, icns::IconType::RGBA32_128x128),
    (256, icns::IconType::RGBA32_128x128_2x),
    (256, icns::IconType::RGBA32_256x256),
    (512, icns::IconType::RGBA32_256x256_2x),
    (512, icns::IconType::RGBA32_512x512),
    (1024, icns::IconType::RGBA32_512x512_2x),
];

/// macOS のアイコンの格子: 1024px の画像の中で、アイコンの本体が占める大きさ。
const MACOS_BODY: f32 = 824.0;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => export(&icons_dir()),
        [command, svg, out] if command == "preview" => {
            let svg = fs::read_to_string(svg).with_context(|| format!("{svg} を読めません"))?;
            let sheet = preview(&svg)?;
            save_png(Path::new(out), &sheet)
        }
        _ => bail!("使い方: gnn-icons (書き出し) | gnn-icons preview <SVG> <PNG>"),
    }
}

/// 元の絵と書き出し先のディレクトリ (このクレートは `tools/icons` にある)。
fn project_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("ワークスペースの中にある")
}

fn icons_dir() -> PathBuf {
    project_root()
        .join("crates")
        .join("gnn-app")
        .join("assets")
        .join("icons")
}

/// 元の絵からアイコン一式を書き出す。
fn export(dir: &Path) -> Result<()> {
    let app = read(&dir.join("app.svg"))?;
    let tray = read(&dir.join("tray.svg"))?;
    let tray_template = read(&dir.join("tray-template.svg"))?;
    let macos = macos_variant(&app)?;

    write_ico(&dir.join("app.ico"), &app)?;
    save_png(&dir.join("app.png"), &render(&app, 512)?)?;
    save_png(&dir.join("tray.png"), &render(&tray, 64)?)?;
    write_icns(&dir.join("app.icns"), &macos)?;
    save_png(&dir.join("tray-template.png"), &render(&tray_template, 36)?)?;
    let readme_dir = project_root().join("docs/images");
    fs::create_dir_all(&readme_dir).context("README の画像のディレクトリを作れません")?;
    save_png(&readme_dir.join("app.png"), &render(&app, 256)?)?;
    println!("{} に書き出しました", dir.display());
    Ok(())
}

fn read(path: &Path) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("{} を読めません", path.display()))
}

/// SVG を正方形の `size` px で描く。
fn render(svg: &str, size: u32) -> Result<RgbaImage> {
    let tree = usvg::Tree::from_str(svg, &usvg::Options::default()).context("SVG を読めません")?;
    let mut pixmap = tiny_skia::Pixmap::new(size, size).context("描く先を作れません")?;
    let view = tree.size();
    let scale = size as f32 / view.width().max(view.height());
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    // tiny-skia は乗算済みのアルファで持つので、PNG に書く前に戻す。
    let mut image = RgbaImage::new(size, size);
    for (out, pixel) in image.pixels_mut().zip(pixmap.pixels()) {
        let color = pixel.demultiply();
        *out = Rgba([color.red(), color.green(), color.blue(), color.alpha()]);
    }
    Ok(image)
}

/// macOS 用に、全面の絵を格子の大きさに縮めて中央に置き、影を付ける。
///
/// 元の絵の `<svg>` の中身をそのまま入れ子にする (元の絵の ID はそのまま使える)。
fn macos_variant(svg: &str) -> Result<String> {
    let open = svg.find("<svg").context("<svg> がありません")?;
    let body_start = svg[open..].find('>').context("<svg> が閉じていません")? + open + 1;
    let body_end = svg.rfind("</svg>").context("</svg> がありません")?;
    let body = &svg[body_start..body_end];
    let scale = MACOS_BODY / 1024.0;
    let offset = (1024.0 - MACOS_BODY) / 2.0;
    Ok(format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024" width="1024" height="1024">
  <defs>
    <filter id="macos-shadow" x="-10%" y="-10%" width="120%" height="125%" color-interpolation-filters="sRGB">
      <feDropShadow dx="0" dy="10" stdDeviation="10" flood-color="#000000" flood-opacity="0.3"/>
    </filter>
  </defs>
  <g filter="url(#macos-shadow)">
    <g transform="translate({offset} {offset}) scale({scale})">{body}</g>
  </g>
</svg>"##
    ))
}

fn write_ico(path: &Path, svg: &str) -> Result<()> {
    let mut frames = Vec::new();
    for size in ICO_SIZES {
        let image = render(svg, size)?;
        frames.push(IcoFrame::as_png(
            image.as_raw(),
            size,
            size,
            ExtendedColorType::Rgba8,
        )?);
    }
    let mut file = BufWriter::new(create(path)?);
    IcoEncoder::new(&mut file).encode_images(&frames)?;
    file.flush()?;
    Ok(())
}

fn write_icns(path: &Path, svg: &str) -> Result<()> {
    let mut family = icns::IconFamily::new();
    for (size, kind) in ICNS_TYPES {
        let image = render(svg, size)?;
        let image = icns::Image::from_data(icns::PixelFormat::RGBA, size, size, image.into_raw())?;
        family.add_icon_with_type(&image, kind)?;
    }
    let mut file = BufWriter::new(create(path)?);
    family.write(&mut file)?;
    file.flush()?;
    Ok(())
}

fn save_png(path: &Path, image: &RgbaImage) -> Result<()> {
    let mut file = BufWriter::new(create(path)?);
    image::codecs::png::PngEncoder::new_with_quality(
        &mut file,
        image::codecs::png::CompressionType::Best,
        image::codecs::png::FilterType::Adaptive,
    )
    .write_image(
        image.as_raw(),
        image.width(),
        image.height(),
        ExtendedColorType::Rgba8,
    )?;
    file.flush()?;
    Ok(())
}

fn create(path: &Path) -> Result<fs::File> {
    fs::File::create(path).with_context(|| format!("{} に書けません", path.display()))
}

/// 確認用の画像: 上の段は明るい背景、下の段は暗い背景に、いくつかの大きさで並べる。
fn preview(svg: &str) -> Result<RgbaImage> {
    const SIZES: [u32; 8] = [16, 24, 32, 48, 64, 128, 256, 512];
    const GAP: u32 = 24;
    let width = SIZES.iter().sum::<u32>() + GAP * (SIZES.len() as u32 + 1);
    let row = SIZES[SIZES.len() - 1] + GAP * 2;
    let mut sheet = RgbaImage::new(width, row * 2);
    for (y, pixel) in sheet.enumerate_rows_mut() {
        let color = if y < row {
            [244, 244, 245, 255]
        } else {
            [24, 24, 27, 255]
        };
        for (_, _, out) in pixel {
            *out = Rgba(color);
        }
    }
    let mut x = GAP;
    for size in SIZES {
        let icon = render(svg, size)?;
        for band in 0..2 {
            let top = band * row + (row - size) / 2;
            image::imageops::overlay(&mut sheet, &icon, i64::from(x), i64::from(top));
        }
        x += size + GAP;
    }
    Ok(sheet)
}
