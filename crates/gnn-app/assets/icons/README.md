# アプリアイコン

元の絵は SVG の 2 つです。ほかのファイルは `mise run icons` (`tools/icons`) がここに書き出します。
絵を変えたら書き出し直し、書き出したファイルもコミットします。

| ファイル | 用途 |
| --- | --- |
| `app.svg` | 元の絵。ゲームのコントローラーを載せた記事のカードと、新着の印。全面に描く |
| `tray-template.svg` | macOS のメニューバーのアイコンの元の絵。黒と透明だけで描く (テンプレート画像) |
| `app.ico` | Windows の実行ファイルとウィンドウのアイコン。16 / 20 / 24 / 30 / 32 / 40 / 48 / 64 / 128 / 256px |
| `app.icns` | macOS のアプリのアイコン。16〜1024px (@1x と @2x)。Apple の格子に合わせて 1024px の中に 824px で置き、影を付ける |
| `app.png` | 512px。README などに使う |
| `tray.png` | Windows のタスクトレイ。64px |
| `tray-template.png` | macOS のメニューバー。36px (18pt の 2 倍) |

Windows では `build.rs` が `assets/app.rc` を通して `app.ico` を実行ファイルのアイコン資源 1 番に入れます
(gpui はウィンドウのアイコンとしてこれを読みます)。macOS では `scripts/bundle-macos.sh` が `app.icns` を
`.app/Contents/Resources/` に置き、`Info.plist` の `CFBundleIconFile` に指定します。

確認用に、絵をいくつかの大きさで並べた画像を作れます (上の段は明るい背景、下の段は暗い背景)。

```bash
cargo run -q -p gnn-icons -- preview crates/gnn-app/assets/icons/app.svg preview.png
```
