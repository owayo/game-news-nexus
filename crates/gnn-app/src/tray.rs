//! タスクトレイ (macOS はメニューバー) のアイコンとメニュー。
//!
//! メニューの操作は [`ServiceEvent`] として画面側へ送る。アイコンは gpui のメインスレッドで
//! 作る (Windows のメッセージループ、macOS の NSApplication の実行ループが同じスレッドで
//! 回っている必要がある)。
//!
//! Windows は左クリックでウィンドウを開き、右クリックでメニューを出す。macOS はメニューバーの
//! 慣習に合わせ、どちらのクリックでもメニューを出す。

use anyhow::{Context as _, Result};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::services::{ServiceEvent, Services};

/// 常駐する場所の呼び名 (Windows はタスクトレイ、macOS はメニューバー)。
/// 文言を `concat!` で組み立てるので、文字列のリテラルに展開するマクロにする。
#[cfg(target_os = "macos")]
macro_rules! resident_place {
    () => {
        "メニューバー"
    };
}
#[cfg(not(target_os = "macos"))]
macro_rules! resident_place {
    () => {
        "トレイ"
    };
}
pub(crate) use resident_place;

/// 設定画面の、ウィンドウを閉じても常駐するかの文言。
pub const CLOSE_TO_TRAY_LABEL: &str = concat!(
    "ウィンドウを閉じても",
    resident_place!(),
    "に残り、取得を続ける"
);
/// 設定画面の、ウィンドウを出さずに起動するかの文言。
pub const START_IN_TRAY_LABEL: &str =
    concat!("起動時はウィンドウを出さず", resident_place!(), "に入れる");

#[cfg(not(target_os = "macos"))]
const TRAY_PNG: &[u8] = include_bytes!("../assets/icons/tray.png");
/// 黒と透明だけのテンプレート画像。メニューバーの明暗に合わせて OS が色を付ける。
/// @2x (1pt = 2px) で描いてある (36px = 18pt)。
#[cfg(any(target_os = "macos", test))]
const TEMPLATE_PNG: &[u8] = include_bytes!("../assets/icons/tray-template.png");
#[cfg(target_os = "macos")]
const TRAY_PNG: &[u8] = TEMPLATE_PNG;

/// 左クリックでメニューを出すか (出さないときは左クリックでウィンドウを開く)。
const MENU_ON_LEFT_CLICK: bool = cfg!(target_os = "macos");

/// メニューバーの高さ (22pt) を @2x の px で表したもの。
#[cfg(any(target_os = "macos", test))]
const MENU_BAR_HEIGHT_PX: u32 = 44;

pub struct Tray {
    /// これを持っている間だけトレイにアイコンが出る。
    icon: TrayIcon,
}

impl Tray {
    pub fn install(services: Services) -> Result<Self> {
        let image = image::load_from_memory(TRAY_PNG)
            .context("トレイのアイコンを読めません")?
            .into_rgba8();
        #[cfg(target_os = "macos")]
        let image = menu_bar_canvas(image);
        let (width, height) = image.dimensions();
        let icon = Icon::from_rgba(image.into_raw(), width, height)
            .context("トレイのアイコンを作れません")?;

        let open = MenuItem::new("開く", true, None);
        let fetch = MenuItem::new("今すぐ取得", true, None);
        let quit = MenuItem::new("終了", true, None);
        let menu = Menu::new();
        menu.append_items(&[&open, &fetch, &PredefinedMenuItem::separator(), &quit])
            .context("トレイのメニューを作れません")?;

        let ids: [(MenuId, Command); 3] = [
            (open.id().clone(), Command::Open),
            (fetch.id().clone(), Command::Fetch),
            (quit.id().clone(), Command::Quit),
        ];
        let menu_services = services.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if let Some((_, command)) = ids.iter().find(|(id, _)| *id == event.id) {
                command.run(&menu_services);
            }
        }));
        // 左クリックでメニューを出すときも Click は届くので、ここで開かないようにする。
        // ハンドラは置いたままにする (置かないと、誰も読まない tray-icon のチャネルにイベントが
        // 溜まり続ける)。
        let click_services = services;
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            let open = !MENU_ON_LEFT_CLICK
                && matches!(
                    event,
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    }
                );
            if open {
                Command::Open.run(&click_services);
            }
        }));

        let builder = TrayIconBuilder::new().with_tooltip("Game News Nexus");
        #[cfg(target_os = "macos")]
        let builder = builder.with_icon_templated(icon);
        #[cfg(not(target_os = "macos"))]
        let builder = builder.with_icon(icon);
        let icon = builder
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(MENU_ON_LEFT_CLICK)
            .build()
            .context("トレイのアイコンを置けません")?;
        Ok(Self { icon })
    }

    /// ツールチップに状況を出す。
    pub fn set_tooltip(&self, text: &str) {
        let _ = self.icon.set_tooltip(Some(text));
    }
}

#[derive(Clone, Copy)]
enum Command {
    Open,
    Fetch,
    Quit,
}

impl Command {
    fn run(self, services: &Services) {
        match self {
            Command::Open => {
                let _ = services.events().send(ServiceEvent::ShowWindow);
            }
            Command::Fetch => services.fetch_now(None),
            Command::Quit => {
                let _ = services.events().send(ServiceEvent::Quit);
            }
        }
    }
}

/// macOS のメニューバーに置く画像にする。
///
/// tray-icon は 22pt より高い画像を 22pt に縮めて描く。@2x で描いたアイコンを 44px (22pt) の
/// 高さの透明な画像の真ん中に置き、描いた大きさ (36px なら 18pt) のまま出るようにする。
/// 高さが 44px を超える画像はそのまま返す (tray-icon が縮める)。
#[cfg(any(target_os = "macos", test))]
fn menu_bar_canvas(icon: image::RgbaImage) -> image::RgbaImage {
    let (width, height) = icon.dimensions();
    let Some(margin) = MENU_BAR_HEIGHT_PX.checked_sub(height) else {
        return icon;
    };
    let mut canvas = image::RgbaImage::new(width + margin, MENU_BAR_HEIGHT_PX);
    let offset = i64::from(margin / 2);
    image::imageops::replace(&mut canvas, &icon, offset, offset);
    canvas
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_icon_is_drawn_at_18pt_on_a_22pt_canvas() {
        let icon = image::load_from_memory(TEMPLATE_PNG).unwrap().into_rgba8();
        assert_eq!(
            icon.dimensions(),
            (36, 36),
            "tray-template.png は 36px (18pt の 2 倍)"
        );
        // テンプレート画像は黒と透明だけで描く (OS は透明度だけを見て色を付ける)。
        assert!(
            icon.pixels()
                .all(|pixel| pixel.0[..3] == [0, 0, 0] || pixel.0[3] == 0)
        );

        let canvas = menu_bar_canvas(icon.clone());
        assert_eq!(canvas.dimensions(), (44, 44));
        for (x, y, pixel) in canvas.enumerate_pixels() {
            let inside = (4..40).contains(&x) && (4..40).contains(&y);
            if inside {
                assert_eq!(pixel, icon.get_pixel(x - 4, y - 4));
            } else {
                assert_eq!(pixel.0[3], 0, "周りは透明 ({x}, {y})");
            }
        }
    }

    #[test]
    fn large_or_wide_icons_keep_their_proportions() {
        let large = image::RgbaImage::new(64, 64);
        assert_eq!(menu_bar_canvas(large).dimensions(), (64, 64));
        let wide = image::RgbaImage::new(60, 36);
        assert_eq!(menu_bar_canvas(wide).dimensions(), (68, 44));
    }

    #[test]
    fn labels_name_the_place_of_this_os() {
        let place = if cfg!(target_os = "macos") {
            "メニューバー"
        } else {
            "トレイ"
        };
        assert!(CLOSE_TO_TRAY_LABEL.contains(place));
        assert!(START_IN_TRAY_LABEL.contains(place));
    }
}
