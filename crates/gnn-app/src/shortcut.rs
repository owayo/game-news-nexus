//! 通知のための、スタートメニューのショートカット (Windows だけ)。
//!
//! Windows は、パッケージ化していないデスクトップアプリのトースト通知を、同じ
//! AppUserModelID を持つスタートメニューのショートカットがあるときにだけ表示する
//! (レジストリに AppUserModelID を登録するだけでは表示されない)。そこで通知を使う設定の
//! とき、この実行ファイルを指すショートカットを `%APPDATA%\Microsoft\Windows\Start Menu\Programs`
//! に作る。すでに同じ実行ファイルを指していれば何もしない。
//!
//! macOS の通知センターはアプリのバンドル (Info.plist の `CFBundleIdentifier`) で通知の出し手を
//! 見分けるので、用意するものはない。`.app` の外から起動したときは通知を出さない (gpui が止める)。

use anyhow::Result;

/// 通知とウィンドウに使う AppUserModelID (Windows)。macOS の bundle id (Info.plist の
/// `CFBundleIdentifier`) と同じ値にする。macOS では使わない。
pub const APP_ID: &str = "io.github.owayo.game-news-nexus";
/// ショートカットとアプリの表示名。
pub const APP_NAME: &str = "Game News Nexus";

/// 設定画面の、通知を使うかの文言。
#[cfg(target_os = "macos")]
pub const NOTIFY_SETTING_LABEL: &str =
    "新着を macOS の通知で知らせる (初めて知らせるときに許可を求めます)";
#[cfg(not(target_os = "macos"))]
pub const NOTIFY_SETTING_LABEL: &str =
    "新着を Windows の通知で知らせる (スタートメニューに登録します)";

/// ショートカットを用意する。作った・作り直したら `true`。
pub fn ensure_start_menu_shortcut() -> Result<bool> {
    imp::ensure()
}

/// 画面を止めないよう、別のスレッドでショートカットを用意する。
///
/// Windows 以外は用意するものがないので何もしない。
pub fn ensure_in_background() {
    if cfg!(not(windows)) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("gnn-shortcut".into())
        .spawn(|| match ensure_start_menu_shortcut() {
            Ok(true) => tracing::info!("通知のためにスタートメニューのショートカットを作りました"),
            Ok(false) => {}
            Err(err) => tracing::warn!(
                "スタートメニューのショートカットを作れません。通知が出ないことがあります: {err:#}"
            ),
        });
    if let Err(err) = spawned {
        tracing::warn!("ショートカットを作るスレッドを起動できません: {err:#}");
    }
}

#[cfg(windows)]
mod imp {
    use std::path::{Path, PathBuf};

    use anyhow::{Context as _, Result};
    use windows::Win32::Foundation::PROPERTYKEY;
    use windows::Win32::System::Com::StructuredStorage::{
        PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0, PropVariantToStringAlloc,
    };
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoTaskMemAlloc, CoTaskMemFree, CoUninitialize, IPersistFile, STGM_READ,
    };
    use windows::Win32::System::Variant::VT_LPWSTR;
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    use windows::core::{GUID, HSTRING, Interface as _, PWSTR};

    use super::{APP_ID, APP_NAME};

    /// System.AppUserModel.ID
    const PKEY_APP_USER_MODEL_ID: PROPERTYKEY = PROPERTYKEY {
        fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
        pid: 5,
    };

    pub fn ensure() -> Result<bool> {
        let appdata = std::env::var_os("APPDATA").context("APPDATA がありません")?;
        let programs = PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs");
        std::fs::create_dir_all(&programs)?;
        let shortcut = programs.join(format!("{APP_NAME}.lnk"));
        let exe = std::env::current_exe().context("実行ファイルの場所が分かりません")?;

        // COM はこの関数を呼んだスレッドで初期化して、終わったら戻す。
        // SAFETY: 初期化に成功したときだけ対になる CoUninitialize を呼ぶ。
        let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
        let result = (|| -> Result<bool> {
            if is_current(&shortcut, &exe).unwrap_or(false) {
                return Ok(false);
            }
            create(&shortcut, &exe)?;
            Ok(true)
        })();
        if initialized {
            // SAFETY: 上の CoInitializeEx が成功したときだけ呼ぶ。
            unsafe { CoUninitialize() };
        }
        result
    }

    /// ショートカットがあり、`exe` を指しているか。
    /// ショートカットがこの実行ファイルを指し、同じ AppUserModelID を持つか。
    /// ID が違う (前の版の ID など) ショートカットでは通知が出ないので、作り直す。
    fn is_current(shortcut: &Path, exe: &Path) -> Result<bool> {
        if !shortcut.exists() {
            return Ok(false);
        }
        // SAFETY: COM のオブジェクトはこの関数の中だけで使い、バッファは十分な長さを渡す。
        // PropVariantToStringAlloc が確保した文字列は、読んだ後に CoTaskMemFree で解放する。
        unsafe {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            let file: IPersistFile = link.cast()?;
            file.Load(&HSTRING::from(shortcut.as_os_str()), STGM_READ)?;
            let mut buffer = [0u16; 1024];
            link.GetPath(&mut buffer, std::ptr::null_mut(), 0)?;
            let length = buffer
                .iter()
                .position(|&ch| ch == 0)
                .unwrap_or(buffer.len());
            let target = PathBuf::from(String::from_utf16_lossy(&buffer[..length]));
            if !same_path(&target, exe) {
                return Ok(false);
            }
            let store: IPropertyStore = link.cast()?;
            let value = store.GetValue(&PKEY_APP_USER_MODEL_ID)?;
            let text = PropVariantToStringAlloc(&value)?;
            let id = text.to_string();
            CoTaskMemFree(Some(text.as_ptr() as *const _));
            Ok(id.is_ok_and(|id| id == APP_ID))
        }
    }

    fn same_path(a: &Path, b: &Path) -> bool {
        match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            (Ok(a), Ok(b)) => a == b,
            _ => a == b,
        }
    }

    fn create(shortcut: &Path, exe: &Path) -> Result<()> {
        // SAFETY: COM のオブジェクトはこの関数の中だけで使う。PROPVARIANT は Drop で解放される。
        unsafe {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            link.SetPath(&HSTRING::from(exe.as_os_str()))?;
            link.SetIconLocation(&HSTRING::from(exe.as_os_str()), 0)?;
            link.SetDescription(&HSTRING::from("ゲームのお知らせを集めて読む"))?;
            if let Some(dir) = exe.parent() {
                link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
            }
            let store: IPropertyStore = link.cast()?;
            store.SetValue(&PKEY_APP_USER_MODEL_ID, &lpwstr(APP_ID)?)?;
            store.Commit()?;
            let file: IPersistFile = link.cast()?;
            file.Save(&HSTRING::from(shortcut.as_os_str()), true)?;
        }
        Ok(())
    }

    /// 文字列の PROPVARIANT (VT_LPWSTR) を作る。AppUserModelID はこの型で渡す必要がある。
    fn lpwstr(value: &str) -> Result<PROPVARIANT> {
        let wide: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
        // SAFETY: CoTaskMemAlloc で確保した領域に終端付きの文字列を写し、PROPVARIANT に渡す。
        // 確保した領域は PROPVARIANT の Drop (PropVariantClear) が CoTaskMemFree で解放する。
        unsafe {
            let pointer = CoTaskMemAlloc(wide.len() * std::mem::size_of::<u16>()) as *mut u16;
            anyhow::ensure!(!pointer.is_null(), "メモリを確保できません");
            std::ptr::copy_nonoverlapping(wide.as_ptr(), pointer, wide.len());
            Ok(PROPVARIANT {
                Anonymous: PROPVARIANT_0 {
                    Anonymous: std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                        vt: VT_LPWSTR,
                        wReserved1: 0,
                        wReserved2: 0,
                        wReserved3: 0,
                        Anonymous: PROPVARIANT_0_0_0 {
                            pwszVal: PWSTR(pointer),
                        },
                    }),
                },
            })
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use anyhow::Result;

    pub fn ensure() -> Result<bool> {
        Ok(false)
    }
}
