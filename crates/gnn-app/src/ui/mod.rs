//! 画面。

pub mod components;
pub mod icon_cropper;
pub mod news;
pub mod releases;
pub mod settings;
pub mod source_editor;
pub mod sources;
pub mod workspace;

pub mod actions {
    //! キーボードやメニューから呼ぶ操作。

    gpui_kit::actions!(
        gnn,
        [
            Refresh,
            FocusSearch,
            SelectNext,
            SelectPrev,
            OpenOriginal,
            ToggleRead,
            MarkAllRead,
            Quit,
            CloseWindow,
            ShowNews,
            ShowReleases,
            ShowSources,
            ShowSettings,
        ]
    );

    use gpui_kit::{App, KeyBinding};

    /// macOS は cmd、ほかの OS は ctrl を付けたキー。
    ///
    /// gpui の `secondary-` は `KeyBinding::new` では使えない (OS を渡さずに解釈するので失敗し、
    /// panic する)。gpui-base と同じく、ビルドする OS で書き分ける。
    #[cfg(target_os = "macos")]
    macro_rules! secondary {
        ($key:literal) => {
            concat!("cmd-", $key)
        };
    }
    #[cfg(not(target_os = "macos"))]
    macro_rules! secondary {
        ($key:literal) => {
            concat!("ctrl-", $key)
        };
    }

    /// 「今すぐ取得」のボタンの説明。キーは `bind_keys` の `secondary!("r")`。
    pub const REFRESH_TOOLTIP: &str = if cfg!(target_os = "macos") {
        "今すぐ取得 (⌘R)"
    } else {
        "今すぐ取得 (Ctrl+R)"
    };

    pub fn bind_keys(cx: &mut App) {
        cx.bind_keys([
            KeyBinding::new(secondary!("r"), Refresh, None),
            KeyBinding::new("f5", Refresh, None),
            KeyBinding::new(secondary!("f"), FocusSearch, None),
            KeyBinding::new(secondary!("q"), Quit, None),
            KeyBinding::new(secondary!("w"), CloseWindow, None),
            KeyBinding::new(secondary!("1"), ShowNews, None),
            KeyBinding::new(secondary!("2"), ShowReleases, None),
            KeyBinding::new(secondary!("3"), ShowSources, None),
            KeyBinding::new(secondary!(","), ShowSettings, None),
            KeyBinding::new("j", SelectNext, Some("NewsList")),
            KeyBinding::new("down", SelectNext, Some("NewsList")),
            KeyBinding::new("k", SelectPrev, Some("NewsList")),
            KeyBinding::new("up", SelectPrev, Some("NewsList")),
            KeyBinding::new("o", OpenOriginal, Some("NewsList")),
            KeyBinding::new("enter", OpenOriginal, Some("NewsList")),
            KeyBinding::new("m", ToggleRead, Some("NewsList")),
            KeyBinding::new("shift-a", MarkAllRead, Some("NewsList")),
        ]);
    }

    #[cfg(test)]
    mod tests {
        use gpui_kit::Keystroke;

        #[test]
        fn secondary_keys_use_cmd_on_macos_and_ctrl_elsewhere() {
            for key in [secondary!("r"), secondary!("q"), secondary!(",")] {
                let keystroke = Keystroke::parse(key).unwrap();
                assert_eq!(keystroke.modifiers.platform, cfg!(target_os = "macos"));
                assert_eq!(keystroke.modifiers.control, !cfg!(target_os = "macos"));
            }
            assert_eq!(Keystroke::parse(secondary!(",")).unwrap().key, ",");
            // KeyBinding::new が使う解釈は secondary- を受け付けない (受け付けるようになったら、
            // secondary! をやめて "secondary-r" と書ける)。
            assert!(Keystroke::parse("secondary-r").is_err());
        }
    }
}
