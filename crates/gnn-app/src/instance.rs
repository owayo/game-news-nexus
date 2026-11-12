//! 二重起動の防止と、後から起動したアプリからの依頼 (ウィンドウを出す・終わる)。
//!
//! 最初に起動したアプリがロックファイルを排他ロックし、プロセスが終わるまで持つ (離さずに終わり、OS が
//! 外す。外れたらプロセスは終わっている)。最初のアプリは localhost で待ち受け、連絡先 (ポート・PID・
//! 実行ファイル・起動ごとの合言葉) をポートのファイルに書く。ロックを取れなかった (後から起動した)
//! アプリは、連絡先を読んで「show」を送り、「ok」の応答を確かめてから終わる。受け取った側はウィンドウを
//! 出す。後から起動したアプリがウィンドウを出さない起動 (`--hidden`、自動起動) なら、何も送らずに終わる。
//!
//! `--quit` で起動したアプリは「quit」を送り、最初のアプリが終わるまで待つ。受け取った側はトレイの
//! 「終了」と同じ通常の終了 (`ServiceEvent::Quit` → app.rs の `Cleanup`) で終わる。終わらせるのは、同じ
//! 実行ファイルで起動したアプリだけ (インストーラーが、入れた場所のアプリを終わらせるつもりで、同じデータの
//! 場所を使う開発中のビルドを終わらせないため)。`--quit` はロックファイルを作らず、起動中かを調べるために
//! 取ったロックはすぐに離すので、起動中のアプリがなければ何も変えずに終わる (アンインストーラーは、一度も
//! 起動していないアプリにも呼ぶ)。
//!
//! 依頼には合言葉を付ける。ポートには同じ PC のほかの利用者もつなげるが、ポートのファイルは利用者の
//! データのディレクトリにあり (Unix は所有者だけが読める)、合言葉の合わない依頼には答えない。読み書きには
//! 期限を付け、少しずつ送ってくる相手にも待ち続けない。
//!
//! ロックとポートは標準ライブラリで扱い、Windows と macOS で同じように動く (ロックは Windows が
//! LockFileEx、macOS が flock。プロセスが落ちても OS が外す)。合言葉は OS の乱数で作り、Windows で
//! プロセスの終わりを待つところだけ Win32 API を使う。macOS で Dock・Finder から開き直したときは
//! LaunchServices が 2 つ目のプロセスを作らないので、ここは通らない (`Application::on_reopen` で
//! 受ける、app.rs)。ここに来るのは端末や launchd から起動したとき。

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::services::ServiceEvent;

/// `--quit` で待つ時間の上限 (依頼を伝える時間を含む)。終わるときの後始末は普通 1 秒もかからない。
pub const QUIT_TIMEOUT: Duration = Duration::from_secs(30);
/// 依頼を送り直す時間。先のアプリが起動の途中なら、ポートのファイルを書くまで少し待つ。
const DELIVERY_WINDOW: Duration = Duration::from_secs(3);
/// 依頼を送り直す・ロックを調べ直す間隔。
const RETRY_INTERVAL: Duration = Duration::from_millis(100);
/// ロックを取れなかったときに、取り直すまで待つ時間。
const LOCK_RETRY: Duration = Duration::from_millis(50);
/// つなぐまでに待つ時間。
const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
/// 依頼・応答の 1 行を読み書きするのにかけてよい時間。
const MESSAGE_TIMEOUT: Duration = Duration::from_secs(2);
/// 依頼・応答の 1 行の大きさの上限。
const MAX_MESSAGE: usize = 256;
/// ポートのファイルの大きさの上限。
const MAX_PORT_FILE: u64 = 4096;
/// ポートのファイルの書式の版。書き方を変えたら上げる。
const PORT_FILE_VERSION: u32 = 1;

pub enum Instance {
    /// このプロセスが最初の起動。`lock` を持っている間、ほかの起動は Secondary になる。
    Primary { lock: File, listener: Listener },
    /// すでに起動しているアプリがある。ウィンドウを出させるなら [`ask_primary_to_show`] を呼ぶ。
    Secondary,
}

/// 最初の起動が依頼を待ち受けるポートと、依頼に付けてもらう合言葉。[`listen`] に渡す。
pub struct Listener {
    socket: TcpListener,
    token: String,
}

/// [`ask_primary_to_quit`] の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuitOutcome {
    /// 起動中のアプリはなかった。何もしていない。
    NotRunning,
    /// 起動中のアプリが終わった (Windows はプロセスが終わり、どちらの OS もロックが外れた)。
    Quit,
    /// 起動中のアプリは、別の実行ファイル (この場所) で起動している。何もしていない。
    OtherExecutable(String),
    /// 起動中のアプリはあるが、依頼を伝えられなかった。
    Unreachable,
    /// 依頼を伝えたが、待つ時間のうちに終わらなかった。
    StillRunning,
}

/// 後から起動したアプリからの依頼。「<種類> <合言葉>」の 1 行で送り、受けた側は「ok <PID>」を返す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Request {
    /// ウィンドウを出す。
    Show,
    /// 通常の終了の手順で終わる (`--quit`)。
    Quit,
}

impl Request {
    const ALL: [Request; 2] = [Request::Show, Request::Quit];

    fn word(self) -> &'static str {
        match self {
            Request::Show => "show",
            Request::Quit => "quit",
        }
    }

    /// 合言葉の合う依頼だけを読む。
    fn parse(line: &str, token: &str) -> Option<Self> {
        let (word, given) = line.trim_end().split_once(' ')?;
        if given != token {
            return None;
        }
        Self::ALL.into_iter().find(|request| request.word() == word)
    }
}

/// 起動中のアプリへの連絡先。最初の起動がポートのファイルに書く。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Endpoint {
    port: u16,
    pid: u32,
    /// 起動ごとの合言葉 (16 バイトの乱数の 16 進)。
    token: String,
    /// 起動した実行ファイルの場所 ([`this_executable`])。分からなければ `None`。
    exe: Option<String>,
}

impl Endpoint {
    fn render(&self) -> String {
        let mut text = format!(
            "version={PORT_FILE_VERSION}\nport={}\npid={}\ntoken={}\n",
            self.port, self.pid, self.token
        );
        if let Some(exe) = &self.exe {
            text.push_str(&format!("exe={exe}\n"));
        }
        text
    }

    /// 版の違うもの・欠けたものは読まない。知らない項目は飛ばす。
    fn parse(text: &str) -> Option<Self> {
        let (mut version, mut port, mut pid, mut token, mut exe) = (None, None, None, None, None);
        for line in text.lines() {
            let (key, value) = line.split_once('=')?;
            match key {
                "version" => version = value.parse::<u32>().ok(),
                "port" => port = value.parse().ok(),
                "pid" => pid = value.parse().ok(),
                "token" => token = Some(value),
                "exe" => exe = Some(value.to_string()),
                _ => {}
            }
        }
        if version? != PORT_FILE_VERSION {
            return None;
        }
        let token = token.filter(|token| is_token(token))?;
        Some(Self {
            port: port?,
            pid: pid?,
            token: token.to_string(),
            exe,
        })
    }

    fn read(port_file: &Path) -> io::Result<Self> {
        let mut text = String::new();
        File::open(port_file)?
            .take(MAX_PORT_FILE)
            .read_to_string(&mut text)?;
        Self::parse(&text).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "ポートのファイルを読めません")
        })
    }

    /// 書きかけを読まれないよう、別の名前で書いてから置き換える。
    fn publish(&self, port_file: &Path) -> io::Result<()> {
        let contents = self.render();
        let mut temp = port_file.as_os_str().to_owned();
        temp.push(format!(".{}.tmp", std::process::id()));
        let temp = PathBuf::from(temp);
        let _ = fs::remove_file(&temp);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        // 合言葉をほかの利用者に読ませない (Windows は、データのディレクトリの権限を継ぐ)。
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let replaced = options
            .open(&temp)
            .and_then(|mut file| file.write_all(contents.as_bytes()))
            .and_then(|()| fs::rename(&temp, port_file));
        if replaced.is_ok() {
            return Ok(());
        }
        let _ = fs::remove_file(&temp);
        // 置き換えられない (Windows で、ほかのプログラムが消せない形で開いているなど) ときは、そのまま書く。
        // 読む側は、読めなければ読み直す。
        fs::write(port_file, contents)
    }
}

fn new_token() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|err| io::Error::other(format!("合言葉の乱数を作れません: {err}")))?;
    Ok(hex::encode(bytes))
}

fn is_token(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// この実行ファイルの場所 (シンボリックリンクなどを解いたもの)。最初の起動が連絡先に書き、`--quit` が
/// 照らす。1 行に 1 項目の書式なので、改行を含む場所は扱わない。
fn this_executable() -> Option<String> {
    let path = fs::canonicalize(std::env::current_exe().ok()?).ok()?;
    let text = path.to_string_lossy().into_owned();
    // Windows の canonicalize が付ける \\?\ は、ドライブ文字の場所なら外す (知らせるときに読みやすく)。
    #[cfg(windows)]
    let text = match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest.to_string(),
        _ => text,
    };
    (!text.contains(['\n', '\r'])).then_some(text)
}

/// 二重起動かどうかを調べる。先に起動しているアプリには何も伝えない。
pub fn acquire(lock_path: &Path, port_file: &Path) -> io::Result<Instance> {
    if let Some(dir) = lock_path.parent() {
        fs::create_dir_all(dir)?;
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path)?;
    // `--quit` が起動中かを調べるために一瞬だけロックを取るのと重なっても、最初の起動を取り逃さない。
    let mut retried = false;
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(TryLockError::WouldBlock) if !retried => {
                retried = true;
                std::thread::sleep(LOCK_RETRY);
            }
            Err(TryLockError::WouldBlock) => return Ok(Instance::Secondary),
            Err(TryLockError::Error(err)) => return Err(err),
        }
    }
    let socket = TcpListener::bind(("127.0.0.1", 0))?;
    let endpoint = Endpoint {
        port: socket.local_addr()?.port(),
        pid: std::process::id(),
        token: new_token()?,
        exe: this_executable(),
    };
    endpoint.publish(port_file)?;
    Ok(Instance::Primary {
        lock,
        listener: Listener {
            socket,
            token: endpoint.token,
        },
    })
}

/// 先に起動しているアプリにウィンドウを出すよう伝える。伝えられたら `true`。
/// 先のアプリが起動の途中なら、ポートのファイルを書くまで少し待つ。
pub fn ask_primary_to_show(port_file: &Path) -> bool {
    let deadline = Instant::now() + DELIVERY_WINDOW;
    loop {
        let sent = Endpoint::read(port_file)
            .and_then(|endpoint| send(&endpoint, Request::Show, deadline))
            .is_ok();
        if sent {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(RETRY_INTERVAL);
    }
}

/// 起動中のアプリに終わるよう伝え、終わるまで最大 `wait` 待つ (`--quit`)。
///
/// ロックファイルは作らず、調べるために取ったロックはすぐに離す。起動中のアプリがなければ
/// 何も変えずに [`QuitOutcome::NotRunning`] を返す。別の実行ファイルで起動したアプリは終わらせない。
pub fn ask_primary_to_quit(
    lock_path: &Path,
    port_file: &Path,
    wait: Duration,
) -> io::Result<QuitOutcome> {
    // 書き込みで開く (読み取りだけで開いたファイルの排他ロックは、OS によっては取れない)。作りはしない。
    let lock = match OpenOptions::new().write(true).open(lock_path) {
        Ok(lock) => lock,
        // 一度も起動していない。
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(QuitOutcome::NotRunning),
        Err(err) => return Err(err),
    };
    if !is_held(&lock)? {
        return Ok(QuitOutcome::NotRunning);
    }
    let own_executable = this_executable();
    let started = Instant::now();
    let deadline = started + wait;
    let deliver_until = started + wait.min(DELIVERY_WINDOW);
    let process = loop {
        if let Ok(endpoint) = Endpoint::read(port_file) {
            if let (Some(theirs), Some(ours)) = (&endpoint.exe, &own_executable)
                && theirs != ours
            {
                return Ok(QuitOutcome::OtherExecutable(theirs.clone()));
            }
            // 送る前に開く。開いている間は PID が使い回されないので、同じ PID からの応答なら、開いたのは
            // 起動中のアプリそのもの。自分自身 (テストで同じプロセスに最初の起動を置いたとき) は待たない。
            let process = (endpoint.pid != std::process::id())
                .then(|| Process::open(endpoint.pid))
                .flatten();
            if send(&endpoint, Request::Quit, deliver_until).is_ok() {
                break process;
            }
        }
        // 送り直しているうちに終わったなら、頼むまでもない。
        if !is_held(&lock)? {
            return Ok(QuitOutcome::NotRunning);
        }
        if Instant::now() >= deliver_until {
            return Ok(QuitOutcome::Unreachable);
        }
        std::thread::sleep(RETRY_INTERVAL);
    };
    // Windows はプロセスの終わりも待つ (ロックはプロセスが終わるときに OS が外すが、外すのが遅れることがある)。
    if let Some(process) = process
        && !process.wait_until(deadline)
    {
        return Ok(QuitOutcome::StillRunning);
    }
    loop {
        if !is_held(&lock)? {
            return Ok(QuitOutcome::Quit);
        }
        if Instant::now() >= deadline {
            return Ok(QuitOutcome::StillRunning);
        }
        std::thread::sleep(RETRY_INTERVAL);
    }
}

/// ほかのプロセス (起動中のアプリ) がロックを持っているか。取れたらすぐに離す。
fn is_held(lock: &File) -> io::Result<bool> {
    match lock.try_lock() {
        Ok(()) => {
            // 閉じるだけだと、Windows は外すのが遅れることがある。
            lock.unlock()?;
            Ok(false)
        }
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(err)) => Err(err),
    }
}

/// 依頼を送り、起動中のアプリの応答を確かめる。つなぐのも読み書きも `deadline` までに済ませる。
fn send(endpoint: &Endpoint, request: Request, deadline: Instant) -> io::Result<()> {
    let address = SocketAddr::from(([127, 0, 0, 1], endpoint.port));
    let mut stream = TcpStream::connect_timeout(&address, time_left(deadline, CONNECT_TIMEOUT)?)?;
    stream.set_write_timeout(Some(time_left(deadline, MESSAGE_TIMEOUT)?))?;
    stream.write_all(format!("{} {}\n", request.word(), endpoint.token).as_bytes())?;
    let reply = read_line(&mut stream, deadline.min(Instant::now() + MESSAGE_TIMEOUT))?;
    if reply.trim_end() == format!("ok {}", endpoint.pid) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Game News Nexus の応答ではありません",
        ))
    }
}

/// 1 行 (改行まで、[`MAX_MESSAGE`] バイトまで) を `deadline` までに読む。読み取りの待ち時間は 1 回の
/// 読み取りごとにしか効かないので、少しずつ送ってくる相手にも期限を過ぎたら読むのをやめるよう、
/// 読むたびに残りの時間を付け直す。
fn read_line(stream: &mut TcpStream, deadline: Instant) -> io::Result<String> {
    let mut line = Vec::new();
    let mut buffer = [0u8; 64];
    loop {
        stream.set_read_timeout(Some(time_left(deadline, MESSAGE_TIMEOUT)?))?;
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        line.extend_from_slice(&buffer[..read]);
        if let Some(end) = line.iter().position(|&byte| byte == b'\n') {
            line.truncate(end);
            break;
        }
        if line.len() > MAX_MESSAGE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "1 行が長すぎます",
            ));
        }
    }
    String::from_utf8(line)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "UTF-8 ではありません"))
}

/// 期限までの残り (`limit` を超えない)。期限を過ぎていればエラー。
fn time_left(deadline: Instant, limit: Duration) -> io::Result<Duration> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        Err(io::ErrorKind::TimedOut.into())
    } else {
        Ok(left.min(limit))
    }
}

/// 後から起動したアプリからの依頼を受け付ける。
pub fn listen(listener: Listener, events: flume::Sender<ServiceEvent>) {
    let Listener { socket, token } = listener;
    let reply = format!("ok {}\n", std::process::id());
    std::thread::Builder::new()
        .name("gnn-instance".into())
        .spawn(move || {
            // 終わるよう頼まれた後は、ウィンドウを出す依頼に答えない (後から起動したアプリは、
            // こちらが終わってから最初の起動になる。main.rs)。
            let mut quitting = false;
            for mut stream in socket.incoming().flatten() {
                let _ = stream.set_write_timeout(Some(MESSAGE_TIMEOUT));
                let Ok(line) = read_line(&mut stream, Instant::now() + MESSAGE_TIMEOUT) else {
                    continue;
                };
                // 合言葉を知らないプログラム (ブラウザなど) には答えない。
                let Some(request) = Request::parse(&line, &token) else {
                    continue;
                };
                let event = match request {
                    Request::Show if quitting => continue,
                    Request::Show => Some(ServiceEvent::ShowWindow),
                    // 重ねて頼まれても、終わるのは 1 度だけ。
                    Request::Quit if quitting => None,
                    Request::Quit => {
                        quitting = true;
                        tracing::info!("後から起動したアプリ (--quit) に頼まれたので終わります");
                        Some(ServiceEvent::Quit)
                    }
                };
                // 終わり始める前に応答が届くよう、知らせるより先に答える。
                let _ = stream.write_all(reply.as_bytes());
                if let Some(event) = event {
                    let _ = events.send(event);
                }
            }
        })
        .expect("待ち受けのスレッドを起動できません");
}

#[cfg(windows)]
mod process {
    use std::time::Instant;

    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };

    /// 終わるのを待つために開いた、ほかのプロセス。開いている間、その PID はほかのプロセスに
    /// 使い回されない。
    pub struct Process(HANDLE);

    impl Process {
        pub fn open(pid: u32) -> Option<Self> {
            // SAFETY: 値を渡すだけで、開けなければエラーが返る。
            unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }
                .ok()
                .map(Self)
        }

        /// 期限までに終わったら `true`。
        pub fn wait_until(&self, deadline: Instant) -> bool {
            let left = deadline.saturating_duration_since(Instant::now());
            // u32::MAX は INFINITE (いつまでも待つ) なので使わない。
            let millis = u32::try_from(left.as_millis())
                .unwrap_or(u32::MAX)
                .min(u32::MAX - 1);
            // SAFETY: open で開き、まだ閉じていないハンドル。
            unsafe { WaitForSingleObject(self.0, millis) == WAIT_OBJECT_0 }
        }
    }

    impl Drop for Process {
        fn drop(&mut self) {
            // SAFETY: open で開いたハンドルを、ここで 1 度だけ閉じる。
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

#[cfg(not(windows))]
mod process {
    use std::time::Instant;

    /// macOS は、終わるアプリのロックを exit() で OS が外す (外れたらプロセスは終わっている) ので、
    /// プロセスは待たず、ロックが外れるのを待つ。
    pub struct Process;

    impl Process {
        pub fn open(_pid: u32) -> Option<Self> {
            Some(Self)
        }

        pub fn wait_until(&self, _deadline: Instant) -> bool {
            true
        }
    }
}

use process::Process;

#[cfg(test)]
mod tests {
    use std::io::{BufRead as _, BufReader};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    /// `child_primary` を、別のプロセスの最初の起動として動かすときのディレクトリ。
    const CHILD_DIR: &str = "GNN_INSTANCE_TEST_CHILD_DIR";

    struct Paths {
        dir: tempfile::TempDir,
        lock: PathBuf,
        port: PathBuf,
    }

    fn paths() -> Paths {
        let dir = tempfile::tempdir().unwrap();
        Paths {
            lock: dir.path().join("instance.lock"),
            port: dir.path().join("instance.port"),
            dir,
        }
    }

    /// 最初の起動になり、依頼を受け付ける。
    fn start_primary(paths: &Paths) -> (File, flume::Receiver<ServiceEvent>) {
        let Instance::Primary { lock, listener } = acquire(&paths.lock, &paths.port).unwrap()
        else {
            panic!("最初の起動は Primary");
        };
        let (sender, receiver) = flume::unbounded();
        listen(listener, sender);
        (lock, receiver)
    }

    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    #[test]
    fn second_launch_asks_the_first_to_show() {
        let paths = paths();
        let (_lock, receiver) = start_primary(&paths);
        assert!(matches!(
            acquire(&paths.lock, &paths.port).unwrap(),
            Instance::Secondary
        ));
        // 調べただけでは伝えない (`--hidden` の 2 つ目はウィンドウを出させずに終わる)。
        assert!(receiver.recv_timeout(Duration::from_millis(300)).is_err());
        assert!(ask_primary_to_show(&paths.port));
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(2)),
            Ok(ServiceEvent::ShowWindow)
        ));
    }

    #[test]
    fn stale_files_do_not_block_a_new_primary() {
        let paths = paths();
        // 前回のプロセスが残したファイル。ロックは持たれていない。
        fs::write(&paths.lock, "").unwrap();
        let unused = TcpListener::bind(("127.0.0.1", 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        fs::write(&paths.port, unused.to_string()).unwrap();
        assert!(matches!(
            acquire(&paths.lock, &paths.port).unwrap(),
            Instance::Primary { .. }
        ));
    }

    #[test]
    fn the_port_file_names_this_launch() {
        let paths = paths();
        let Instance::Primary { lock, listener } = acquire(&paths.lock, &paths.port).unwrap()
        else {
            panic!("最初の起動は Primary");
        };
        let endpoint = Endpoint::read(&paths.port).unwrap();
        assert_eq!(endpoint.pid, std::process::id());
        assert_eq!(endpoint.port, listener.socket.local_addr().unwrap().port());
        assert_eq!(endpoint.token, listener.token);
        assert!(is_token(&endpoint.token));
        assert!(endpoint.exe.is_some());
        assert_eq!(endpoint.exe, this_executable());
        // 書きかけの一時ファイルを残さない。
        let mut names: Vec<_> = fs::read_dir(paths.dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(names, ["instance.lock", "instance.port"]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(&paths.port).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "合言葉はほかの利用者に読ませない");
        }

        // 合言葉は起動ごとに作り直す。
        drop((lock, listener));
        let Instance::Primary { .. } = acquire(&paths.lock, &paths.port).unwrap() else {
            panic!("ロックを離した後の起動は Primary");
        };
        assert_ne!(Endpoint::read(&paths.port).unwrap().token, endpoint.token);
    }

    #[test]
    fn port_files_of_other_versions_are_not_read() {
        let endpoint = Endpoint {
            port: 51234,
            pid: 42,
            token: "0123456789abcdef0123456789abcdef".to_string(),
            exe: Some(r"C:\Program Files\x=y\game-news-nexus.exe".to_string()),
        };
        assert_eq!(Endpoint::parse(&endpoint.render()), Some(endpoint.clone()));
        let without_exe = Endpoint {
            exe: None,
            ..endpoint.clone()
        };
        assert_eq!(
            Endpoint::parse(&without_exe.render()),
            Some(without_exe.clone())
        );
        let rendered = endpoint.render();
        assert_eq!(
            Endpoint::parse(&rendered.replace("version=1", "version=2")),
            None
        );
        assert_eq!(
            Endpoint::parse(&rendered.replace(&endpoint.token, "short")),
            None
        );
        // 前の書式 (ポート番号だけ)。
        assert_eq!(Endpoint::parse("51234"), None);
    }

    #[test]
    fn quit_asks_the_first_to_quit_and_waits_until_it_lets_go() {
        let paths = paths();
        let (lock, receiver) = start_primary(&paths);
        let released = Arc::new(AtomicBool::new(false));
        // 起動中のアプリの代わり。終わるよう頼まれたら、後始末に少しかかってからロックを離す。
        let primary = std::thread::spawn({
            let released = released.clone();
            move || {
                let event = receiver.recv_timeout(Duration::from_secs(10));
                std::thread::sleep(Duration::from_millis(300));
                released.store(true, Ordering::SeqCst);
                drop(lock);
                event
            }
        });
        assert_eq!(
            ask_primary_to_quit(&paths.lock, &paths.port, Duration::from_secs(10)).unwrap(),
            QuitOutcome::Quit
        );
        // 「ok」の応答だけで戻らず、ロックが離れるまで待つ。
        assert!(released.load(Ordering::SeqCst));
        assert!(matches!(primary.join().unwrap(), Ok(ServiceEvent::Quit)));
        // 終わった後は、次の起動が最初の起動になる。
        assert!(matches!(
            acquire(&paths.lock, &paths.port).unwrap(),
            Instance::Primary { .. }
        ));
    }

    /// 別のプロセスで動く最初の起動。`quit_waits_for_another_process_to_end` が、このテストだけを
    /// 走らせる子のプロセスとして起動する。環境変数がなければ (普通にテストを走らせたときは) 何もしない。
    #[test]
    fn child_primary() {
        let Some(dir) = std::env::var_os(CHILD_DIR) else {
            return;
        };
        let dir = PathBuf::from(dir);
        let Instance::Primary { lock, listener } =
            acquire(&dir.join("instance.lock"), &dir.join("instance.port")).unwrap()
        else {
            panic!("子のプロセスは最初の起動");
        };
        let (sender, receiver) = flume::unbounded();
        listen(listener, sender);
        fs::write(dir.join("ready"), "").unwrap();
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(30)),
            Ok(ServiceEvent::Quit)
        ));
        // 後始末に少しかかってからロックを離し、さらに少したってからプロセスが終わる。
        std::thread::sleep(Duration::from_millis(300));
        drop(lock);
        std::thread::sleep(Duration::from_millis(300));
    }

    #[test]
    fn quit_waits_for_another_process_to_end() {
        let paths = paths();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "instance::tests::child_primary",
                "--exact",
                "--test-threads=1",
            ])
            .env(CHILD_DIR, paths.dir.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let ready = paths.dir.path().join("ready");
        let deadline = Instant::now() + Duration::from_secs(30);
        while !ready.exists() {
            if let Some(status) = child.try_wait().unwrap() {
                panic!("子のプロセスが先に終わりました: {status}");
            }
            assert!(Instant::now() < deadline, "子のプロセスが起動しません");
            std::thread::sleep(Duration::from_millis(50));
        }
        let started = Instant::now();
        assert_eq!(
            ask_primary_to_quit(&paths.lock, &paths.port, Duration::from_secs(30)).unwrap(),
            QuitOutcome::Quit
        );
        assert!(started.elapsed() >= Duration::from_millis(300));
        // Windows は、ロックが外れた後もプロセスが終わるまで待つ。
        if cfg!(windows) {
            assert!(
                child.try_wait().unwrap().is_some(),
                "子のプロセスが終わる前に戻りました"
            );
        }
        assert!(child.wait().unwrap().success());
    }

    #[test]
    fn quit_without_a_running_app_changes_nothing() {
        let paths = paths();
        // 一度も起動していない。ロックもポートのファイルも作らない。
        assert_eq!(
            ask_primary_to_quit(&paths.lock, &paths.port, Duration::from_secs(1)).unwrap(),
            QuitOutcome::NotRunning
        );
        assert!(!paths.lock.exists());
        assert!(!paths.port.exists());

        // 前回のアプリが残したファイル。ロックは持たれていない。
        fs::write(&paths.lock, "").unwrap();
        fs::write(&paths.port, "1").unwrap();
        assert_eq!(
            ask_primary_to_quit(&paths.lock, &paths.port, Duration::from_secs(1)).unwrap(),
            QuitOutcome::NotRunning
        );
        assert_eq!(fs::read_to_string(&paths.port).unwrap(), "1");
        // 調べるために取ったロックは離してある。
        assert!(matches!(
            acquire(&paths.lock, &paths.port).unwrap(),
            Instance::Primary { .. }
        ));
    }

    #[test]
    fn quit_leaves_an_app_started_from_another_executable() {
        let paths = paths();
        let (_lock, receiver) = start_primary(&paths);
        // 同じデータの場所を使う、別の場所の実行ファイル (開発中のビルドなど) で起動したアプリ。
        let elsewhere = r"C:\elsewhere\game-news-nexus.exe".to_string();
        let endpoint = Endpoint {
            exe: Some(elsewhere.clone()),
            ..Endpoint::read(&paths.port).unwrap()
        };
        fs::write(&paths.port, endpoint.render()).unwrap();
        assert_eq!(
            ask_primary_to_quit(&paths.lock, &paths.port, Duration::from_secs(1)).unwrap(),
            QuitOutcome::OtherExecutable(elsewhere)
        );
        assert!(receiver.recv_timeout(Duration::from_millis(300)).is_err());
    }

    #[test]
    fn quit_reports_a_first_launch_that_does_not_end() {
        let paths = paths();
        let (_lock, receiver) = start_primary(&paths);
        let started = Instant::now();
        assert_eq!(
            ask_primary_to_quit(&paths.lock, &paths.port, Duration::from_millis(500)).unwrap(),
            QuitOutcome::StillRunning
        );
        assert!(started.elapsed() >= Duration::from_millis(500));
        assert!(matches!(receiver.try_recv(), Ok(ServiceEvent::Quit)));
    }

    #[test]
    fn quit_reports_a_first_launch_that_does_not_answer() {
        let paths = paths();
        // ロックは持たれているが、ポートで誰も待ち受けていない。
        let Instance::Primary {
            lock: _lock,
            listener,
        } = acquire(&paths.lock, &paths.port).unwrap()
        else {
            panic!("最初の起動は Primary");
        };
        drop(listener);
        assert_eq!(
            ask_primary_to_quit(&paths.lock, &paths.port, Duration::from_millis(500)).unwrap(),
            QuitOutcome::Unreachable
        );
    }

    #[test]
    fn requests_without_the_token_get_no_answer() {
        let paths = paths();
        let (_lock, receiver) = start_primary(&paths);
        let port = Endpoint::read(&paths.port).unwrap().port;
        // 合言葉なし・違う合言葉・ブラウザなどからの接続。答えずに閉じ、アプリには何も伝えない。
        for request in [
            "quit\n",
            "quit 0123456789abcdef0123456789abcdef\n",
            "GET /quit HTTP/1.1\r\n\r\n",
        ] {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream.write_all(request.as_bytes()).unwrap();
            let mut reply = String::new();
            let _ = BufReader::new(stream).read_line(&mut reply);
            assert_eq!(reply, "", "{request:?} に答えました");
        }
        assert!(receiver.recv_timeout(Duration::from_millis(300)).is_err());
    }

    #[test]
    fn show_is_refused_once_asked_to_quit() {
        let paths = paths();
        let (_lock, receiver) = start_primary(&paths);
        let endpoint = Endpoint::read(&paths.port).unwrap();
        send(&endpoint, Request::Quit, soon()).unwrap();
        // 終わる途中のアプリはウィンドウを出さない (後から起動したアプリは、終わるのを待って最初の起動になる)。
        assert!(send(&endpoint, Request::Show, soon()).is_err());
        // 重ねて頼まれても答えるが、終わるのは 1 度だけ。
        send(&endpoint, Request::Quit, soon()).unwrap();
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(2)),
            Ok(ServiceEvent::Quit)
        ));
        assert!(receiver.recv_timeout(Duration::from_millis(300)).is_err());
    }

    /// 改行を送らずに 1 バイトずつ送り続ける相手。
    fn trickle(mut stream: TcpStream) {
        while stream.write_all(b"x").is_ok() {
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    #[test]
    fn a_slow_sender_does_not_hold_the_first_launch() {
        let paths = paths();
        let (_lock, receiver) = start_primary(&paths);
        let endpoint = Endpoint::read(&paths.port).unwrap();
        let stream = TcpStream::connect(("127.0.0.1", endpoint.port)).unwrap();
        std::thread::spawn(move || trickle(stream));
        // 1 行を読むのは 2 秒 (MESSAGE_TIMEOUT) で打ち切り、次の依頼に答える。送る側も応答を 2 秒しか
        // 待たないので、打ち切りまでの残りが 1 秒ほどになってから送る。
        std::thread::sleep(Duration::from_secs(1));
        let started = Instant::now();
        send(
            &endpoint,
            Request::Show,
            Instant::now() + Duration::from_secs(10),
        )
        .unwrap();
        assert!(started.elapsed() < MESSAGE_TIMEOUT + Duration::from_secs(1));
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(2)),
            Ok(ServiceEvent::ShowWindow)
        ));
    }

    #[test]
    fn a_slow_answer_does_not_hold_the_second_launch() {
        // 応答を少しずつ送り続ける相手 (ポートを使い回したほかのプログラムなど)。
        let server = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let endpoint = Endpoint {
            port: server.local_addr().unwrap().port(),
            pid: 42,
            token: "0123456789abcdef0123456789abcdef".to_string(),
            exe: None,
        };
        std::thread::spawn(move || {
            if let Ok((stream, _)) = server.accept() {
                trickle(stream);
            }
        });
        let started = Instant::now();
        assert!(
            send(
                &endpoint,
                Request::Quit,
                Instant::now() + Duration::from_millis(500)
            )
            .is_err()
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
