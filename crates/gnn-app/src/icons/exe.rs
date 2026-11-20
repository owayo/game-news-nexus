//! Windows のシェルで exe の先頭のアイコンを読み、透過を保った RGBA 画像にする。

use std::path::Path;

use anyhow::{Context as _, Result, ensure};
use image::{DynamicImage, RgbaImage};
use windows::Win32::Foundation::{RPC_E_CHANGED_MODE, S_FALSE};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppRGBA, IWICImagingFactory,
    WICConvertBitmapSource,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::UI::{Shell::SHDefExtractIconW, WindowsAndMessaging::HICON};
use windows::core::{HSTRING, Owned};

pub(super) fn extract(path: &Path) -> Result<DynamicImage> {
    // SAFETY: COM をこのスレッドで使い、成功時 (S_FALSE を含む) だけ同じスレッドで解除する。
    // 既に別のモードで初期化されていれば、そのまま使う。
    let init = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if init != RPC_E_CHANGED_MODE {
        init.ok()?;
    }
    let result = read_icon(path);
    if init.is_ok() {
        unsafe { CoUninitialize() };
    }
    result
}

fn read_icon(path: &Path) -> Result<DynamicImage> {
    // SAFETY: パスと出力先は呼び出し中有効。取得した HICON は Owned で解放し、
    // COM オブジェクトは初期化したスレッド内で破棄する。
    unsafe {
        let mut icon = HICON::default();
        let status = SHDefExtractIconW(
            &HSTRING::from(path.as_os_str()),
            0,
            0,
            Some(&mut icon),
            None,
            super::ICON_SIZE,
        );
        status.ok()?;
        ensure!(
            status != S_FALSE && !icon.is_invalid(),
            "埋め込みアイコンがありません"
        );
        let icon = Owned::new(icon);
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let bitmap = factory.CreateBitmapFromHICON(*icon)?;
        let rgba = WICConvertBitmapSource(&GUID_WICPixelFormat32bppRGBA, &bitmap)?;
        let (mut width, mut height) = (0, 0);
        rgba.GetSize(&mut width, &mut height)?;
        ensure!(
            width > 0 && height > 0 && width <= super::ICON_SIZE && height <= super::ICON_SIZE,
            "アイコンの寸法が不正です"
        );
        let mut pixels = RgbaImage::new(width, height);
        rgba.CopyPixels(std::ptr::null(), width * 4, &mut pixels)
            .context("アイコンの画素を読み込めません")?;
        Ok(DynamicImage::ImageRgba8(pixels))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_embedded_application_icon() {
        // build.rs がテストの実行ファイルにも同じアイコン資源を埋め込む。
        let icon = extract(&std::env::current_exe().unwrap())
            .unwrap()
            .into_rgba8();
        assert!(icon.pixels().any(|px| px[3] == 0));
        assert!(icon.pixels().any(|px| px[3] == 255));
    }

    #[test]
    fn rejects_file_without_an_icon() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("empty.EXE");
        std::fs::write(&file, []).unwrap();
        assert!(crate::icons::load_file(&file).is_err());
    }
}
