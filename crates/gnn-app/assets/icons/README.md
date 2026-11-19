# アプリアイコン

元の絵は SVG の 3 つです。アプリの SVG には、提供された 512px の PNG を埋め込んでいます。ほかのファイルは `mise run icons` (`tools/icons`) がここに書き出します。
絵を変えたら書き出し直し、書き出したファイルもコミットします。

| ファイル | 用途 |
| --- | --- |
| `app.svg` | 提供された PNG を埋め込んだ元の絵。赤いコントローラーと記事のカード。透明な角を含めてそのまま使う |
| `tray.svg` | Windows のトレイ専用の絵。16px の格子で輪郭とボタンを描いた、赤 1 色のコントローラー |
| `tray-template.svg` | macOS のメニューバーのアイコンの元の絵。黒と透明だけで描く (テンプレート画像) |
| `app.ico` | Windows の実行ファイルとウィンドウのアイコン。16〜256px。表示倍率 100〜250% の 16・24・32px に相当するサイズを収録 |
| `app.icns` | macOS のアプリのアイコン。16〜1024px (@1x と @2x)。Apple の格子に合わせて 1024px の中に 824px で置き、影を付ける |
| `app.png` | 512px。画面の中のロゴの原画。`ui::components::AppLogo` が画面の倍率に応じた画素数に縮小し、同じ大きさの画像を使い回す |
| `tray.png` | Windows のタスクトレイ。64px |
| `tray-template.png` | macOS のメニューバー。36px (18pt の 2 倍) |

配布アイコンと画面内のロゴは、色をリニア RGB に変換し、透明度を掛けてから Lanczos3 で縮小します。元の PNG は 512px なので、それを超える大きさは拡大して書き出します。

README のロゴ `docs/images/app.png` (256px) も、同じコマンドで書き出します。

Windows では `build.rs` が `assets/app.rc` を通して `app.ico` を実行ファイルのアイコン資源 1 番に入れます
(gpui はウィンドウのアイコンとしてこれを読みます)。macOS では `scripts/bundle-macos.sh` が `app.icns` を
`.app/Contents/Resources/` に置き、`Info.plist` の `CFBundleIconFile` に指定します。

確認用に、絵をいくつかの大きさで並べた画像を作れます (上の段は明るい背景、下の段は暗い背景)。

```bash
cargo run -q -p gnn-icons -- preview crates/gnn-app/assets/icons/app.svg preview.png
```
