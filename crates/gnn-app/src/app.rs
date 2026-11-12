//! 画面側の起動。gpui を立ち上げ、サービスからの知らせを画面へ流す。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gnn_core::ThemePreference;
use gnn_core::source::Channel;
use gpui_kit::component::{Theme, ThemeMode, TitleBar};
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Entity, Global, QuitMode, SharedString,
    SystemNotification, Window, WindowBounds, WindowOptions, px, size,
};
use tracing_appender::non_blocking::WorkerGuard;

use crate::http_client::{CachingHttpClient, ImageLoader};
use crate::services::{AddedEntry, SHUTDOWN_TIMEOUT, ServiceEvent, Services, StartupNotes};
use crate::shortcut;
use crate::state::{AppEvent, AppState};
use crate::tray::Tray;
use crate::ui::actions::{Quit, bind_keys};
use crate::ui::workspace::Workspace;

/// 開いているメインウィンドウ。閉じたら `None`。
#[derive(Default)]
struct MainWindow(Option<(AnyWindowHandle, Entity<Workspace>)>);

impl Global for MainWindow {}

struct TrayHolder(Tray);

impl Global for TrayHolder {}

/// 終わるときの後始末。1 度だけ行う。終了の操作 (トレイの「終了」・Ctrl+Q・`--quit`・
/// 残さない設定でウィンドウを閉じる) は、どれも `cx.quit()` からここに来る。
///
/// 定期取得と予定の読み取りを止め、取得の通信を取り消し、ストアへの書き込みが済むのを待って
/// ストアを閉じてから ([`Services::shutdown`]、長くても [`SHUTDOWN_TIMEOUT`])、ログを書き切る
/// (書き切るのは tracing-appender が 1 秒ほどまで待つ)。
///
/// Windows は `run` から戻った後に行う。macOS は終了が `run` から戻らずに exit() するので、
/// 終わる直前の知らせ (`on_app_quit`、`applicationWillTerminate` の中) で行う。
#[derive(Clone)]
struct Cleanup(Rc<RefCell<Option<CleanupTargets>>>);

struct CleanupTargets {
    services: Services,
    /// 落とすと、溜まっているログを書き切ってから書き込みのスレッドを止める。
    log_guard: Option<WorkerGuard>,
}

impl Cleanup {
    fn new(services: Services, log_guard: Option<WorkerGuard>) -> Self {
        Self(Rc::new(RefCell::new(Some(CleanupTargets {
            services,
            log_guard,
        }))))
    }

    fn run(&self) {
        let taken = self.0.borrow_mut().take();
        if let Some(CleanupTargets {
            services,
            log_guard,
        }) = taken
        {
            services.shutdown(SHUTDOWN_TIMEOUT);
            drop(log_guard);
        }
    }
}

pub fn run(
    services: Services,
    events: flume::Receiver<ServiceEvent>,
    start_hidden: bool,
    notes: StartupNotes,
    log_guard: Option<WorkerGuard>,
) {
    let cleanup = Cleanup::new(services.clone(), log_guard);
    let application = gpui_kit::application().with_assets(gpui_kit::assets::AllAssets);
    #[cfg(target_os = "macos")]
    let application = {
        let application = crate::macos::configure(application, start_hidden);
        // Dock・Finder・Launchpad から開き直したとき。LaunchServices は 2 つ目を起動せず、
        // 起動中のこのアプリに知らせる (ウィンドウが 1 つも出ていないときだけ呼ばれる)。
        application.on_reopen(|cx| {
            if cx.has_global::<MainWindow>() {
                open_main_window(cx);
            }
        });
        application
    };
    #[cfg(target_os = "macos")]
    let quit_cleanup = cleanup.clone();
    application.run(move |cx| {
        gpui_kit::init(cx);
        cx.set_app_identity(shortcut::APP_ID, shortcut::APP_NAME);
        if services.settings().notify_new_entries {
            shortcut::ensure_in_background();
        }
        // ウィンドウを閉じても終わらず、トレイから終了するまで取得を続ける。
        cx.set_quit_mode(QuitMode::Explicit);
        let images = CachingHttpClient::new(
            services.http_client(),
            services.runtime(),
            services.images().clone(),
            Arc::new(services.clone()),
            services.events(),
            services.stop_signal(),
        );
        cx.set_http_client(images.clone());
        cx.set_global(ImageLoader(images.clone()));
        bind_keys(cx);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.set_global(MainWindow::default());
        #[cfg(target_os = "macos")]
        {
            crate::macos::init(cx);
            // macOS は終わっても run から戻らないので、終わる直前に後始末をする。返す future は
            // gpui が 200ms (gpui の SHUTDOWN_TIMEOUT) しか待たないので、待つのはこの同期の部分で
            // 行う (applicationWillTerminate の中なので、長くても services::SHUTDOWN_TIMEOUT)。
            cx.on_app_quit(move |_| {
                quit_cleanup.run();
                async {}
            })
            .detach();
        }

        let app_state = AppState::init(services.clone(), cx);
        if let Some(migrated) = &notes.migrated {
            let notice = format!(
                "取得元の設定を新しい形式に移しました (購読 {} 件・手元の定義 {} 件)。元のファイルは {} に残しています。",
                migrated.subscriptions,
                migrated.local_sources.len(),
                migrated.backup.display()
            );
            tracing::info!("{notice}");
            app_state.update(cx, |state, _| state.set_notice(notice));
        }
        apply_theme(services.settings().theme, cx);

        match Tray::install(services.clone()) {
            Ok(tray) => {
                cx.set_global(TrayHolder(tray));
                // 未読数と取得中かどうかをトレイのツールチップに出す。
                cx.observe(&app_state, |state, cx| {
                    let state = state.read(cx);
                    let unread = state.unread_total(Channel::News);
                    let text = match state.running_count() {
                        0 if unread > 0 => format!("Game News Nexus - 未読 {unread} 件"),
                        0 => "Game News Nexus".to_string(),
                        running => format!("Game News Nexus - 取得中 ({running})"),
                    };
                    cx.global::<TrayHolder>().0.set_tooltip(&text);
                })
                .detach();
            }
            Err(err) => tracing::warn!("トレイにアイコンを置けません: {err:#}"),
        }

        cx.on_system_notification_response(|response, cx| {
            open_main_window(cx);
            if let Some((channel, id)) = parse_tag(&response.tag) {
                reveal(id, channel, cx);
            }
        });

        cx.subscribe(&app_state, |_, event, cx| {
            if let AppEvent::NewEntries(added) = event {
                notify_new_entries(added, cx);
            }
        })
        .detach();

        cx.on_window_closed(|cx, window_id| {
            let closed = cx
                .global::<MainWindow>()
                .0
                .as_ref()
                .is_some_and(|(handle, _)| handle.window_id() == window_id);
            if closed {
                cx.global_mut::<MainWindow>().0 = None;
                #[cfg(target_os = "macos")]
                crate::macos::main_window_closed(cx);
            }
        })
        .detach();

        let pump_state = app_state.clone();
        cx.spawn(async move |cx| {
            while let Ok(event) = events.recv_async().await {
                cx.update(|cx| match event {
                    ServiceEvent::ShowWindow => open_main_window(cx),
                    ServiceEvent::Quit => cx.quit(),
                    // gpui が覚えている失敗した画像・古い画像を読み直させる。
                    ServiceEvent::ImagesCached { available, changed } => {
                        images.reload_cached(&available, &changed, cx)
                    }
                    ServiceEvent::RetryImages(due) => images.retry(&due, cx),
                    ServiceEvent::ImagesDiscarded(keys) => images.reload_discarded(&keys, cx),
                    event => {
                        pump_state.update(cx, |state, cx| state.handle_service_event(event, cx))
                    }
                });
            }
        })
        .detach();

        services.spawn_scheduler();
        if !start_hidden {
            open_main_window(cx);
        }
    });
    cleanup.run();
}

/// テーマを設定に合わせる。
pub fn apply_theme(preference: ThemePreference, cx: &mut App) {
    match preference {
        ThemePreference::System => Theme::sync_system_appearance(None, cx),
        ThemePreference::Light => Theme::change(ThemeMode::Light, None, cx),
        ThemePreference::Dark => Theme::change(ThemeMode::Dark, None, cx),
    }
}

/// メインウィンドウを閉じる (Ctrl+W、macOS は ⌘W とメニューの「閉じる」)。閉じるボタンと同じく、
/// 「ウィンドウを閉じても残る」を切っていればアプリを終える。
pub fn close_main_window(window: &mut Window, cx: &mut App) {
    if quits_on_close(cx) {
        cx.quit();
    } else {
        window.remove_window();
    }
}

/// ウィンドウを閉じたらアプリを終えるか (「ウィンドウを閉じても残る」を切っている)。
fn quits_on_close(cx: &App) -> bool {
    !AppState::global(cx).read(cx).settings().close_to_tray
}

/// メインウィンドウを出す。開いていれば前に出す。
pub fn open_main_window(cx: &mut App) {
    #[cfg(target_os = "macos")]
    crate::macos::will_show_main_window(cx);
    if let Some((handle, _)) = cx.global::<MainWindow>().0.clone() {
        if handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
        {
            return;
        }
        cx.global_mut::<MainWindow>().0 = None;
    }

    let app_state = AppState::global(cx);
    let mut window_size = size(px(1360.), px(860.));
    if let Some(display) = cx.primary_display() {
        let display_size = display.bounds().size;
        window_size.width = window_size.width.min(display_size.width * 0.9);
        window_size.height = window_size.height.min(display_size.height * 0.9);
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            window_size,
            cx,
        ))),
        window_min_size: Some(size(px(720.), px(480.))),
        ..TitleBar::window_options()
    };

    let opened = gpui_kit::open_window(options, cx, |window, cx| {
        window.set_window_title("Game News Nexus");
        window.on_window_should_close(cx, |_, cx| {
            if quits_on_close(cx) {
                cx.quit();
            }
            true
        });
        let workspace = cx.new(|cx| Workspace::new(app_state.clone(), window, cx));
        let focus = gpui_kit::Focusable::focus_handle(workspace.read(cx), cx);
        window.focus(&focus, cx);
        workspace
    });
    match opened {
        Ok((handle, workspace)) => {
            let notice = app_state.update(cx, |state, _| state.take_notice());
            let _ = handle.update(cx, |_, window, cx| {
                window.activate_window();
                if let Some(notice) = notice {
                    use gpui_kit::component::WindowExt as _;
                    window.push_notification(
                        gpui_kit::component::notification::Notification::info(notice),
                        cx,
                    );
                }
            });
            cx.global_mut::<MainWindow>().0 = Some((handle, workspace.clone()));
            #[cfg(debug_assertions)]
            {
                open_dev_page(handle, &workspace, cx);
                open_dev_cropper(handle, cx);
            }
        }
        Err(err) => tracing::error!("ウィンドウを開けません: {err:#}"),
    }
}

/// 開発用: `GNN_DEV_PAGE` (news・releases・sources・settings) で起動時の画面を、
/// `GNN_DEV_SOURCE` でソース画面で選ぶソースを決める。画面を操作せずに確かめるため。
#[cfg(debug_assertions)]
fn open_dev_page(handle: AnyWindowHandle, workspace: &Entity<Workspace>, cx: &mut App) {
    use crate::ui::workspace::Page;
    let page = match std::env::var("GNN_DEV_PAGE").ok().as_deref() {
        Some("news") => Page::News,
        Some("sources") => Page::Sources,
        Some("settings") => Page::Settings,
        _ => return,
    };
    let source = std::env::var("GNN_DEV_SOURCE").ok();
    let workspace = workspace.clone();
    let _ = handle.update(cx, |_, window, cx| {
        workspace.update(cx, |workspace, cx| match source {
            Some(id) if page == Page::Sources => workspace.open_source(&id, window, cx),
            _ => workspace.show_page(page, cx),
        })
    });
}

/// 開発用: `GNN_DEV_CROP_IMAGE` に画像のパスを指定すると、起動時に先頭のソースの
/// アイコンの切り抜き画面を開く。ファイル選択を経ずに切り抜き画面を確かめるため。
#[cfg(debug_assertions)]
fn open_dev_cropper(handle: AnyWindowHandle, cx: &mut App) {
    let Some(image) = std::env::var_os("GNN_DEV_CROP_IMAGE") else {
        return;
    };
    let app_state = AppState::global(cx);
    let Some(source) = app_state
        .read(cx)
        .sources()
        .first()
        .map(|source| source.config.clone())
    else {
        return;
    };
    let icons_dir = app_state.read(cx).services().paths().icons_dir();
    let _ = handle.update(cx, |_, window, cx| {
        crate::ui::icon_cropper::open_cropper(
            std::path::PathBuf::from(image),
            source.id.clone(),
            source.name.clone(),
            icons_dir,
            window,
            cx,
            |path, _, _| tracing::info!("開発用の切り抜き: {}", path.display()),
        );
    });
}

fn reveal(entry_id: i64, channel: Channel, cx: &mut App) {
    if let Some((handle, workspace)) = cx.global::<MainWindow>().0.clone() {
        let _ = handle.update(cx, |_, window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.reveal_entry(entry_id, channel, window, cx)
            })
        });
    }
}

/// 新着を OS の通知で知らせる。
fn notify_new_entries(added: &[AddedEntry], cx: &mut App) {
    let app_state = AppState::global(cx);
    if !app_state.read(cx).settings().notify_new_entries || added.is_empty() {
        return;
    }
    let first = &added[0];
    let title: SharedString = if added.len() == 1 {
        format!("{} の新着", first.source_name).into()
    } else {
        format!("新着 {} 件", added.len()).into()
    };
    let mut lines: Vec<String> = added
        .iter()
        .take(3)
        .map(|entry| {
            if added.len() == 1 {
                entry.title.clone()
            } else {
                format!("・{} {}", entry.source_name, entry.title)
            }
        })
        .collect();
    if added.len() > 3 {
        lines.push(format!("ほか {} 件", added.len() - 3));
    }
    cx.show_system_notification(SystemNotification {
        tag: format!("gnn/{}/{}", first.channel.as_str(), first.entry_id).into(),
        title,
        body: lines.join("\n").into(),
        actions: Vec::new(),
    });
}

fn parse_tag(tag: &str) -> Option<(Channel, i64)> {
    let mut parts = tag.strip_prefix("gnn/")?.split('/');
    let channel = Channel::parse(parts.next()?)?;
    let id = parts.next()?.parse().ok()?;
    Some((channel, id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_notification_tags() {
        assert_eq!(parse_tag("gnn/news/42"), Some((Channel::News, 42)));
        assert_eq!(parse_tag("gnn/releases/7"), None);
        assert_eq!(parse_tag("other/news/1"), None);
        assert_eq!(parse_tag("gnn/news/x"), None);
    }
}
