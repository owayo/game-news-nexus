//! サインイン (ログイン) したときに自動で起動する設定。どちらも `--hidden` 付きで起動する。
//!
//! - Windows: 現在のユーザーの Run キー (`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`) に
//!   登録する。タスクマネージャーの「スタートアップ」からも外せるので、設定ファイルではなく
//!   レジストリを正本として毎回読む。
//! - macOS: `~/Library/LaunchAgents/io.github.owayo.game-news-nexus.plist` を書く/消す。launchd が
//!   次のログインで読み、この実行ファイルを起動する。いま launchd に読ませる (`launchctl bootstrap`)
//!   と、その場でもう 1 つ起動してウィンドウを出してしまうので読ませない。ファイルを正本として
//!   毎回読み、この実行ファイルを指しているかを確かめる。

use anyhow::Result;

/// 設定画面の文言。
#[cfg(target_os = "macos")]
pub const SETTING_LABEL: &str = "ログインしたら自動で起動する (メニューバーに入れた状態で)";
#[cfg(not(target_os = "macos"))]
pub const SETTING_LABEL: &str = "Windows にサインインしたら自動で起動する (トレイに入れた状態で)";

#[cfg(windows)]
mod imp {
    use anyhow::{Context as _, Result};

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE_NAME: &str = "GameNewsNexus";

    fn command() -> Result<String> {
        let exe = std::env::current_exe().context("実行ファイルの場所が分かりません")?;
        Ok(format!("\"{}\" --hidden", exe.display()))
    }

    pub fn is_enabled() -> bool {
        windows_registry::CURRENT_USER
            .open(RUN_KEY)
            .and_then(|key| key.get_string(VALUE_NAME))
            .is_ok_and(|value| command().is_ok_and(|command| value.eq_ignore_ascii_case(&command)))
    }

    pub fn set_enabled(enabled: bool) -> Result<()> {
        let key = windows_registry::CURRENT_USER
            .create(RUN_KEY)
            .context("自動起動の設定を開けません")?;
        if enabled {
            key.set_string(VALUE_NAME, command()?)
                .context("自動起動を登録できません")?;
        } else if key.get_string(VALUE_NAME).is_ok() {
            key.remove_value(VALUE_NAME)
                .context("自動起動を解除できません")?;
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::path::PathBuf;

    use anyhow::{Context as _, Result};

    use super::launch_agent;

    /// この実行ファイルの、シンボリックリンクをたどった絶対パス。
    /// 端末から相対パスで起動したときも、launchd に渡せる形にする。
    fn exe() -> Result<PathBuf> {
        let exe = std::env::current_exe().context("実行ファイルの場所が分かりません")?;
        std::fs::canonicalize(&exe)
            .with_context(|| format!("実行ファイル {} の場所を確かめられません", exe.display()))
    }

    fn plist_path() -> Result<PathBuf> {
        let home = std::env::home_dir().context("ホームディレクトリが分かりません")?;
        Ok(launch_agent::path_in(&home))
    }

    pub fn is_enabled() -> bool {
        match (plist_path(), exe()) {
            (Ok(path), Ok(exe)) => launch_agent::is_registered(&path, &exe),
            _ => false,
        }
    }

    pub fn set_enabled(enabled: bool) -> Result<()> {
        let path = plist_path()?;
        if enabled {
            launch_agent::register(&path, &exe()?)
        } else {
            launch_agent::unregister(&path)
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod imp {
    use anyhow::Result;

    pub fn is_enabled() -> bool {
        false
    }

    pub fn set_enabled(_enabled: bool) -> Result<()> {
        anyhow::bail!("この OS では自動起動の設定に対応していません")
    }
}

/// macOS の LaunchAgent (launchd のジョブ定義)。テストのため Windows でもテストのときはビルドする。
#[cfg(any(target_os = "macos", test))]
mod launch_agent {
    use std::fs;
    use std::path::{Path, PathBuf};

    use anyhow::{Context as _, Result, bail};

    /// ジョブの名前。アプリのバンドル ID (Info.plist の `CFBundleIdentifier`) と同じにする。
    pub const LABEL: &str = "io.github.owayo.game-news-nexus";

    /// `~/Library/LaunchAgents/<LABEL>.plist`。
    pub fn path_in(home: &Path) -> PathBuf {
        home.join("Library")
            .join("LaunchAgents")
            .join(format!("{LABEL}.plist"))
    }

    /// 書き出す plist。ログイン時に `exe --hidden` を 1 度だけ起動する (落ちても起動し直さない)。
    /// `AssociatedBundleIdentifiers` は、システム設定の「ログイン項目」にアプリとして出すため。
    pub fn plist(exe: &Path) -> Result<String> {
        let exe = exe
            .to_str()
            .context("実行ファイルの場所に UTF-8 でない文字が含まれています")?;
        if exe.chars().any(char::is_control) {
            bail!("実行ファイルの場所に制御文字が含まれていて、自動起動に登録できません");
        }
        // 隔離されたまま (ダウンロードした場所から) 開くと、起動のたびに変わる一時的な場所で動く。
        if exe.contains("/AppTranslocation/") {
            bail!(
                "アプリが一時的な場所で動いているため、自動起動に登録できません。アプリを「アプリケーション」フォルダへ移してから開き直してください"
            );
        }
        let exe = escape(exe);
        Ok(format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{exe}</string>
		<string>--hidden</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>LimitLoadToSessionType</key>
	<string>Aqua</string>
	<key>AssociatedBundleIdentifiers</key>
	<array>
		<string>{LABEL}</string>
	</array>
</dict>
</plist>
"#
        ))
    }

    /// XML の文字列に書けるようにする。
    fn escape(text: &str) -> String {
        let mut escaped = String::with_capacity(text.len());
        for ch in text.chars() {
            match ch {
                '&' => escaped.push_str("&amp;"),
                '<' => escaped.push_str("&lt;"),
                '>' => escaped.push_str("&gt;"),
                '"' => escaped.push_str("&quot;"),
                '\'' => escaped.push_str("&apos;"),
                _ => escaped.push(ch),
            }
        }
        escaped
    }

    /// `path` に、`exe` を起動する内容がそのまま書いてあるか。
    pub fn is_registered(path: &Path, exe: &Path) -> bool {
        let Ok(expected) = plist(exe) else {
            return false;
        };
        fs::read_to_string(path).is_ok_and(|current| current == expected)
    }

    pub fn register(path: &Path, exe: &Path) -> Result<()> {
        let text = plist(exe)?;
        if fs::read_to_string(path).is_ok_and(|current| current == text) {
            return Ok(());
        }
        let dir = path
            .parent()
            .context("自動起動の設定の置き場所が分かりません")?;
        fs::create_dir_all(dir).with_context(|| format!("{} を作成できません", dir.display()))?;
        // 書きかけのファイルを launchd に読ませないよう、隣に書いてから置き換える。
        // launchd は拡張子が .plist のファイルだけを読む。
        let temp = path.with_extension("plist.tmp");
        let written = fs::write(&temp, text)
            .and_then(|()| restrict_permissions(&temp))
            .and_then(|()| fs::rename(&temp, path));
        if let Err(err) = written {
            let _ = fs::remove_file(&temp);
            return Err(err).context("自動起動を登録できません");
        }
        Ok(())
    }

    pub fn unregister(path: &Path) -> Result<()> {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err).context("自動起動を解除できません"),
        }
    }

    /// launchd は、ほかの利用者が書き換えられる定義を読まない。umask によらず 644 にする。
    #[cfg(unix)]
    fn restrict_permissions(path: &Path) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o644))
    }

    #[cfg(not(unix))]
    fn restrict_permissions(_path: &Path) -> std::io::Result<()> {
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use pretty_assertions::assert_eq;

        const EXE: &str = "/Applications/Game News Nexus.app/Contents/MacOS/game-news-nexus";

        #[test]
        fn writes_a_launch_agent_that_starts_hidden_at_login() {
            assert_eq!(
                path_in(Path::new("/Users/me")),
                Path::new("/Users/me")
                    .join("Library")
                    .join("LaunchAgents")
                    .join("io.github.owayo.game-news-nexus.plist")
            );
            let text = plist(Path::new(EXE)).unwrap();
            assert_eq!(
                text,
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                 <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
                 <plist version=\"1.0\">\n\
                 <dict>\n\
                 \t<key>Label</key>\n\
                 \t<string>io.github.owayo.game-news-nexus</string>\n\
                 \t<key>ProgramArguments</key>\n\
                 \t<array>\n\
                 \t\t<string>/Applications/Game News Nexus.app/Contents/MacOS/game-news-nexus</string>\n\
                 \t\t<string>--hidden</string>\n\
                 \t</array>\n\
                 \t<key>RunAtLoad</key>\n\
                 \t<true/>\n\
                 \t<key>LimitLoadToSessionType</key>\n\
                 \t<string>Aqua</string>\n\
                 \t<key>AssociatedBundleIdentifiers</key>\n\
                 \t<array>\n\
                 \t\t<string>io.github.owayo.game-news-nexus</string>\n\
                 \t</array>\n\
                 </dict>\n\
                 </plist>\n"
            );
        }

        #[test]
        fn escapes_the_path_and_refuses_unusable_ones() {
            let text = plist(Path::new("/Users/me/A&B <x>/Game's \"app\"")).unwrap();
            assert!(text.contains(
                "<string>/Users/me/A&amp;B &lt;x&gt;/Game&apos;s &quot;app&quot;</string>"
            ));
            let translocated = "/private/var/folders/ab/cd/T/AppTranslocation/0A1B/d/Game News Nexus.app/Contents/MacOS/game-news-nexus";
            assert!(plist(Path::new(translocated)).is_err());
            assert!(plist(Path::new("/Applications/a\nb")).is_err());
        }

        #[test]
        fn registers_and_unregisters_the_file() {
            let home = tempfile::tempdir().unwrap();
            let path = path_in(home.path());
            let exe = Path::new(EXE);
            assert!(!is_registered(&path, exe));
            // 消すものがなくても失敗にしない。
            unregister(&path).unwrap();

            register(&path, exe).unwrap();
            assert!(is_registered(&path, exe));
            assert_eq!(fs::read_to_string(&path).unwrap(), plist(exe).unwrap());
            assert!(!path.with_extension("plist.tmp").exists());
            // 同じ内容なら書き直さない。2 度目も成功する。
            register(&path, exe).unwrap();
            // アプリを移したら、前の場所を指す登録は「登録していない」扱い。
            let moved = Path::new(
                "/Users/me/Applications/Game News Nexus.app/Contents/MacOS/game-news-nexus",
            );
            assert!(!is_registered(&path, moved));
            register(&path, moved).unwrap();
            assert!(is_registered(&path, moved));

            unregister(&path).unwrap();
            assert!(!path.exists());
            assert!(!is_registered(&path, moved));
        }
    }
}

/// 自動起動が、この実行ファイルを指して登録されているか。
pub fn is_enabled() -> bool {
    imp::is_enabled()
}

/// 自動起動を登録・解除する。
pub fn set_enabled(enabled: bool) -> Result<()> {
    imp::set_enabled(enabled)
}
