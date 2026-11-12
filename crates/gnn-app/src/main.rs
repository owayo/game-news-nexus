//! Game News Nexus: ゲームのお知らせを集めて読む常駐型デスクトップアプリ。

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod autostart;
mod format;
mod http_client;
mod icons;
mod image_recovery;
mod instance;
#[cfg(target_os = "macos")]
mod macos;
mod services;
mod shortcut;
mod state;
mod tray;
mod ui;

use std::process::ExitCode;

use anyhow::Result;
use gnn_core::{AppPaths, RunStatus};

use crate::instance::{Instance, QuitOutcome};
use crate::services::Services;

const USAGE: &str = concat!(
    "\
Game News Nexus

使い方:
  game-news-nexus [オプション]

オプション:
  --hidden       ウィンドウを出さずに",
    tray::resident_place!(),
    "で起動する
  --fetch-once   画面を出さずに有効なソースを 1 度ずつ取得して終わる
  --quit         この実行ファイルで起動中のアプリを終了させ、終わるまで最大 30 秒待つ
                 (終了コードは 0 = 終わった・起動していなかった、1 = 伝えられない・終わらない・
                 別の実行ファイルで起動している)
  -h, --help     この説明を出す

--hidden・--fetch-once・--quit は一緒に使えません。

環境変数:
  GNN_HOME       設定とデータを置くディレクトリ (既定は OS 標準の場所)
  RUST_LOG       ログの詳しさ (例: info, gnn_app=debug)
"
);

/// 起動のしかた。オプションで 1 つだけ選ぶ。
#[derive(Debug, PartialEq, Eq)]
enum Mode {
    /// 常駐する。`hidden` ならウィンドウを出さずに起動する (`--hidden`)。
    Resident { hidden: bool },
    /// 画面を出さずに 1 度ずつ取得して終わる (`--fetch-once`)。
    FetchOnce,
    /// 起動中のアプリを終わらせる (`--quit`)。
    Quit,
}

/// 引数を読む。`--help` なら `None`。不明なオプションと組み合わせの誤りは、並びによらず誤りにする。
fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Option<Mode>, String> {
    let (mut hidden, mut fetch_once, mut quit, mut help) = (false, false, false, false);
    for arg in args {
        match arg.as_str() {
            "--hidden" => hidden = true,
            "--fetch-once" => fetch_once = true,
            "--quit" => quit = true,
            "-h" | "--help" => help = true,
            // 古い macOS は、Finder から開いたアプリにプロセスの番号 (-psn_0_12345) を渡す。
            #[cfg(target_os = "macos")]
            other if other.starts_with("-psn_") => {}
            other => return Err(format!("不明なオプション: {other}")),
        }
    }
    if help {
        return Ok(None);
    }
    match (hidden, fetch_once, quit) {
        (hidden, false, false) => Ok(Some(Mode::Resident { hidden })),
        (false, true, false) => Ok(Some(Mode::FetchOnce)),
        (false, false, true) => Ok(Some(Mode::Quit)),
        _ => Err("--hidden・--fetch-once・--quit は一緒に使えません".to_string()),
    }
}

fn main() -> ExitCode {
    let mode = match parse_args(std::env::args().skip(1)) {
        Ok(Some(mode)) => mode,
        Ok(None) => {
            attach_console();
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            attach_console();
            eprintln!("{message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(mode) {
        Ok(code) => code,
        Err(err) => {
            tracing::error!("起動できません: {err:#}");
            eprintln!("起動できません: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(mode: Mode) -> Result<ExitCode> {
    let paths = AppPaths::resolve()?;
    let hidden = match mode {
        Mode::Quit => {
            attach_console();
            return Ok(quit_running_app(&paths));
        }
        Mode::FetchOnce => {
            paths.ensure()?;
            // 取得の結果を出し終えてから落として、ログを書き切る。
            let _log_guard = init_logging(&paths, true);
            attach_console();
            return fetch_once(paths);
        }
        Mode::Resident { hidden } => hidden,
    };
    paths.ensure()?;
    // app::run に渡し、終わるときの後始末で書き切る。
    let log_guard = init_logging(&paths, false);

    // `instance_lock` はプロセスが終わるまで持ち続ける (離すと二重起動を防げなくなる)。
    let lock_file = paths.lock_file();
    let port_file = paths.instance_port_file();
    let (instance_lock, listener) = match instance::acquire(&lock_file, &port_file)? {
        Instance::Primary { lock, listener } => (lock, listener),
        Instance::Secondary if hidden => {
            // 自動起動などのウィンドウを出さない起動。起動中のアプリはそのまま常駐を続ける。
            tracing::info!("すでに起動しているため終わります");
            return Ok(ExitCode::SUCCESS);
        }
        Instance::Secondary => {
            if instance::ask_primary_to_show(&port_file) {
                tracing::info!("すでに起動しているため、そちらのウィンドウを出します");
                return Ok(ExitCode::SUCCESS);
            }
            // 起動中のアプリが終わる途中 (--quit やトレイの「終了」の直後) だったなら、こちらが最初の起動になる。
            match instance::acquire(&lock_file, &port_file)? {
                Instance::Primary { lock, listener } => (lock, listener),
                Instance::Secondary => {
                    tracing::warn!(
                        "すでに起動しているアプリに、ウィンドウを出すよう伝えられませんでした"
                    );
                    return Ok(ExitCode::SUCCESS);
                }
            }
        }
    };

    let (sender, receiver) = flume::unbounded();
    instance::listen(listener, sender.clone());
    let (services, notes) = Services::start(paths, sender)?;
    let hidden = hidden || services.settings().start_in_tray;
    tracing::info!("起動しました (トレイのみ: {hidden})");
    app::run(services, receiver, hidden, notes, log_guard);
    // ロックは離さずに終わり、プロセスが終わるときに OS が外す (macOS は run から戻らずに終わる)。
    // `--quit` は、ロックが外れたらプロセスが終わったとみなす。
    std::mem::forget(instance_lock);
    Ok(ExitCode::SUCCESS)
}

/// `--quit`: 起動中のアプリに終了を頼み、終わるまで待つ。
///
/// 頼むだけなので、データのディレクトリもログのファイルも作らない (アンインストーラーは、一度も
/// 起動していないアプリにも呼ぶ)。結果は標準出力・標準エラーにだけ出す。
fn quit_running_app(paths: &AppPaths) -> ExitCode {
    let outcome = instance::ask_primary_to_quit(
        &paths.lock_file(),
        &paths.instance_port_file(),
        instance::QUIT_TIMEOUT,
    );
    match outcome {
        Ok(QuitOutcome::NotRunning) => {
            println!("起動中の Game News Nexus はありません");
            ExitCode::SUCCESS
        }
        Ok(QuitOutcome::Quit) => {
            println!("Game News Nexus を終了しました");
            ExitCode::SUCCESS
        }
        Ok(QuitOutcome::OtherExecutable(exe)) => {
            eprintln!(
                "起動中の Game News Nexus は別の実行ファイル ({exe}) で起動しているので、終了させません (その実行ファイルの --quit を使ってください)"
            );
            ExitCode::FAILURE
        }
        Ok(QuitOutcome::Unreachable) => {
            eprintln!("起動中の Game News Nexus に終了を伝えられませんでした");
            ExitCode::FAILURE
        }
        Ok(QuitOutcome::StillRunning) => {
            eprintln!(
                "Game News Nexus に終了を伝えましたが、{} 秒たっても終わりません",
                instance::QUIT_TIMEOUT.as_secs()
            );
            ExitCode::FAILURE
        }
        Err(err) => {
            eprintln!("Game News Nexus が起動しているかを調べられません: {err}");
            ExitCode::FAILURE
        }
    }
}

fn fetch_once(paths: AppPaths) -> Result<ExitCode> {
    let (sender, _receiver) = flume::unbounded();
    let (services, notes) = Services::start(paths, sender)?;
    if let Some(migrated) = &notes.migrated {
        println!(
            "旧形式の sources.toml を移しました (購読 {} 件・手元の定義 {} 件、元のファイルは {})",
            migrated.subscriptions,
            migrated.local_sources.len(),
            migrated.backup.display()
        );
    }
    let summaries = services.fetch_once_blocking();
    // 取得と予定の読み取りをすべて済ませたので、常駐のときと同じ手順でストアを閉じる。
    services.shutdown(services::SHUTDOWN_TIMEOUT);
    let mut failed = 0;
    for summary in &summaries {
        let status = match summary.status {
            RunStatus::Ok => "OK  ",
            RunStatus::Partial => "一部",
            RunStatus::Failed => {
                failed += 1;
                "失敗"
            }
        };
        println!(
            "{status} {:<28} 一覧 {:>3} 件 / 新着 {:>3} 件 / 更新 {:>3} 件{}",
            summary.source_id,
            summary.listed,
            summary.added,
            summary.updated,
            summary
                .message
                .as_deref()
                .map(|message| format!("  {message}"))
                .unwrap_or_default()
        );
    }
    println!("{} ソースを取得しました (失敗 {failed})", summaries.len());
    Ok(if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn init_logging(
    paths: &AppPaths,
    to_stderr: bool,
) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let appender = tracing_appender::rolling::daily(paths.logs_dir(), "game-news-nexus.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(writer);
    let stderr_layer = (to_stderr || cfg!(debug_assertions))
        .then(|| tracing_subscriber::fmt::layer().with_writer(std::io::stderr));
    tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stderr_layer)
        .try_init()
        .ok()?;
    Some(guard)
}

/// リリース版は画面だけのアプリなので、コマンドラインから呼ばれたときは親のコンソールにつなぐ。
#[cfg(all(windows, not(debug_assertions)))]
fn attach_console() {
    unsafe extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
    }
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    // SAFETY: 引数は定数で、失敗しても何も起きない。
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(all(windows, not(debug_assertions))))]
fn attach_console() {}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Option<Mode>, String> {
        parse_args(args.iter().map(|arg| arg.to_string()))
    }

    #[test]
    fn parses_the_options() {
        assert_eq!(parse(&[]), Ok(Some(Mode::Resident { hidden: false })));
        assert_eq!(
            parse(&["--hidden"]),
            Ok(Some(Mode::Resident { hidden: true }))
        );
        assert_eq!(parse(&["--fetch-once"]), Ok(Some(Mode::FetchOnce)));
        assert_eq!(parse(&["--quit"]), Ok(Some(Mode::Quit)));
        assert_eq!(parse(&["--quit", "--quit"]), Ok(Some(Mode::Quit)));
        assert_eq!(parse(&["--help"]), Ok(None));
    }

    #[test]
    fn modes_do_not_mix() {
        // 起動中のアプリを終わらせるだけで、自分は常駐も取得もしない。
        assert!(parse(&["--quit", "--hidden"]).is_err());
        assert!(parse(&["--fetch-once", "--quit"]).is_err());
        assert!(parse(&["--hidden", "--fetch-once"]).is_err());
        // --help は組み合わせによらず説明を出す。不明なオプションは並びによらず誤り。
        assert_eq!(parse(&["--quit", "--hidden", "--help"]), Ok(None));
        assert!(parse(&["--help", "--unknown"]).is_err());
        assert!(parse(&["--unknown", "--help"]).is_err());
    }
}
