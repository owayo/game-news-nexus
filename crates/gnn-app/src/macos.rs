//! macOS だけの扱い: Dock のアイコン (アプリの起動方針)、アプリのメニュー、標準のキー操作。
//!
//! Windows のトレイ常駐に合わせ、メインウィンドウを開いている間は Dock とメニューバーに
//! アプリを出し ([`ActivationPolicy::Regular`])、ウィンドウを閉じてメニューバーのアイコン
//! だけで常駐する間は Dock から消す ([`ActivationPolicy::Accessory`])。Accessory の間は
//! アプリのメニューも出ない。
//!
//! 2 つ目の起動のうち、Dock・Finder・Launchpad から開いたものは LaunchServices が新しい
//! プロセスを作らず、起動中のアプリに reopen を送る (`Application::on_reopen`、app.rs)。
//! 端末や launchd から起動したものは、ほかの OS と同じく instance.rs が受け持つ。

use gpui_kit::component::input;
use gpui_kit::{
    ActivationPolicy, App, Application, KeyBinding, Menu, MenuItem, OsAction, SystemMenuType,
    Window, WindowingRequest,
};

use crate::shortcut::APP_NAME;
use crate::ui::actions::{
    CloseWindow, FocusSearch, Quit, Refresh, ShowNews, ShowSettings, ShowSources,
};

gpui_kit::actions!(gnn_macos, [Hide, HideOthers, ShowAll, Minimize, Zoom]);

/// `run` の前に済ませる設定。
pub fn configure(application: Application, start_hidden: bool) -> Application {
    if start_hidden {
        // メニューバーに入れて起動するときは、Dock にアイコンを出さずに始める。
        // run の中で切り替えると、起動の途中で Dock にアイコンが一瞬出る。
        application.with_windowing(WindowingRequest::Headless)
    } else {
        application
    }
}

/// アプリのメニューと、そこから呼ぶ操作を用意する。メニューに出すショートカットは
/// キーの割り当てから決まるので、`bind_keys` の後に呼ぶ。
pub fn init(cx: &mut App) {
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &Minimize, cx| with_active_window(cx, |window| window.minimize_window()));
    cx.on_action(|_: &Zoom, cx| with_active_window(cx, |window| window.zoom_window()));
    cx.bind_keys([
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, None),
    ]);
    cx.set_menus(menus());
}

/// 前面のウィンドウを操作する。キーやメニューの操作を処理している間、gpui はそのウィンドウを
/// 取り出していて `update` できない (`window not found` になる) ので、処理が済んでから行う。
fn with_active_window(cx: &mut App, operate: impl FnOnce(&mut Window) + 'static) {
    cx.defer(move |cx| {
        if let Some(window) = cx.active_window() {
            let _ = window.update(cx, |_, window, _| operate(window));
        }
    });
}

/// メインウィンドウを出す前に呼ぶ。Dock とメニューバーにアプリを出し、前に出す。
pub fn will_show_main_window(cx: &mut App) {
    cx.set_activation_policy(ActivationPolicy::Regular);
    // Regular にしただけではメニューバーが切り替わらないことがあるので、自分で前に出す。
    cx.activate(true);
}

/// メインウィンドウを閉じた後に呼ぶ。ほかにウィンドウがなければ Dock から消し、
/// メニューバーのアイコンだけで常駐する。最小化しただけのときは呼ばれない。
pub fn main_window_closed(cx: &mut App) {
    if cx.windows().is_empty() {
        cx.set_activation_policy(ActivationPolicy::Accessory);
    }
}

/// アプリのメニュー。項目の名前は macOS の日本語の標準に合わせる。使えない項目
/// (ウィンドウがないときの「設定…」など) は gpui が灰色にする。
fn menus() -> Vec<Menu> {
    vec![
        Menu::new(APP_NAME).items([
            MenuItem::action("設定…", ShowSettings),
            MenuItem::separator(),
            MenuItem::os_submenu("サービス", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action(format!("{APP_NAME} を非表示"), Hide),
            MenuItem::action("ほかを非表示", HideOthers),
            MenuItem::action("すべてを表示", ShowAll),
            MenuItem::separator(),
            MenuItem::action(format!("{APP_NAME} を終了"), Quit),
        ]),
        Menu::new("編集").items([
            MenuItem::os_action("取り消す", input::Undo, OsAction::Undo),
            MenuItem::os_action("やり直す", input::Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("カット", input::Cut, OsAction::Cut),
            MenuItem::os_action("コピー", input::Copy, OsAction::Copy),
            MenuItem::os_action("ペースト", input::Paste, OsAction::Paste),
            MenuItem::os_action("すべてを選択", input::SelectAll, OsAction::SelectAll),
        ]),
        Menu::new("表示").items([
            MenuItem::action("ニュース", ShowNews),
            MenuItem::action("ソース", ShowSources),
            MenuItem::separator(),
            MenuItem::action("検索", FocusSearch),
            MenuItem::action("今すぐ取得", Refresh),
        ]),
        // gpui は名前が "Window" のメニューだけを OS のウインドウメニューにする (ウインドウの
        // 一覧が付く)。ウインドウは 1 つなので、日本語の名前のまま普通のメニューとして置く。
        Menu::new("ウインドウ").items([
            MenuItem::action("しまう", Minimize),
            MenuItem::action("拡大/縮小", Zoom),
            MenuItem::separator(),
            MenuItem::action("閉じる", CloseWindow),
        ]),
    ]
}
