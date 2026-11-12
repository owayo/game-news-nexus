<p align="center">
  <img src="docs/images/app.png" width="128" alt="Game News Nexus">
</p>

<h1 align="center">Game News Nexus</h1>

<p align="center">
  ゲームのお知らせを集め、メンテナンスやイベントの予定を読み取る Windows・macOS 向け常駐型デスクトップアプリ
</p>

<h3 align="center">対応プラットフォーム</h3>

<p align="center">
  <img src="https://img.shields.io/badge/macOS-000000?logo=apple&amp;logoColor=white" alt="macOS">
  <img src="https://img.shields.io/badge/Windows-0078D6" alt="Windows">
</p>

<p align="center">
  <a href="https://github.com/owayo/game-news-nexus/actions/workflows/ci.yml"><img src="https://github.com/owayo/game-news-nexus/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI"></a>
  <a href="https://github.com/owayo/game-news-nexus/releases/latest"><img src="https://img.shields.io/github/v/release/owayo/game-news-nexus" alt="Release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/owayo/game-news-nexus" alt="License"></a>
</p>

---

公式のお知らせを集めて読み、記事に書かれたメンテナンスやイベントの予定を確認できます。読みたいゲームを選ぶと、カタログの取得定義を使って定期的に収集します。Rust と [gpui](https://gpui.rs)（[gpui-kit](https://gpui-kit.com)）で作っています。

## 機能

| 機能 | 内容 |
| --- | --- |
| お知らせの収集と閲覧 | RSS / Atom、JSON API、HTML の記事を収集し、新しい順の一覧と本文を並べます。画像も表示できます。 |
| 予定の読み取り | 題名と本文からメンテナンスやイベントの日時を読み取り、終了・延長・中止・延期を反映します。根拠の記述も確認できます。 |
| 今日・明日の予定 | 近い予定の記事を上にまとめ、サイドバーにもメンテナンスの印を出します。本文を取り直して追記も拾います。 |
| 検索と既読管理 | 全文検索と一致箇所の強調、ゲームごとの未読数に対応します。全角・半角、大文字・小文字、半角カナの違いを区別しません。 |
| 常駐と通知 | ウィンドウを閉じても収集を続け、新着を通知します。自動起動やゲームごとの取得間隔も設定できます。 |
| 手元の取得定義 | カタログにないゲームを追加し、既存の定義を自分用に調整できます。画像を切り抜いてゲームのアイコンにも設定できます。 |

## インストール

配布版は [Releases](https://github.com/owayo/game-news-nexus/releases) の添付から取得します。公開済みの版がない場合は、[ソースから動かす手順](docs/development.md#ソースから動かす)を使ってください。添付のチェックサムは `SHA256SUMS` にあります。

| 対応環境 | 配布物 |
| --- | --- |
| macOS（Apple Silicon） | `game-news-nexus-<版>-macos-arm64.dmg` |
| Windows 10 / 11（x64） | `game-news-nexus-<版>-windows-x64-setup.exe` |

コード署名と Apple の公証はしていないため、初回起動時に OS の確認が出ます。

### Windows

インストーラーを実行します。管理者権限は不要です。`%LOCALAPPDATA%\Programs\Game News Nexus` に入り、スタートメニューにショートカットを作ります。「Windows によって PC が保護されました」と出たら、「詳細情報」から「実行」を選びます。

更新時には起動中のアプリへ終了を頼み、処理が終わってからファイルを入れ替えます。アンインストールは Windows の「設定」から行えます。設定と記事のデータは残ります。

### macOS

DMG を開き、`Game News Nexus` を `Applications` へドラッグしてから起動します。DMG の中やダウンロードフォルダから直接起動した場合は、自動起動を登録できません。

初回起動が止められた場合は、「システム設定」→「プライバシーとセキュリティ」から起動を許可します。アンインストールする場合は、アプリの設定で自動起動を切ってから終了し、`/Applications/Game News Nexus.app` を削除します。

## 使い方

1. アプリを起動し、「ソース」画面で読みたいゲームを on にします。
2. 記事を選ぶと本文が開き、既読になります。タイトルや本文の検索、予定の確認もできます。
3. 設定画面で取得間隔・通知・自動起動を調整します。ウィンドウを閉じたあとの動作も設定できます。

初めて起動したときは、DMM GAMES と HoYoverse のグループのゲームが on です。ゲームメディアや Steam のニュースもソース画面で選べます。取得定義は別リポジトリの [game-news-nexus-config](https://github.com/owayo/game-news-nexus-config) で管理し、起動時と 1 日 1 回取り込みます。off にしたゲームは、定義が更新されても off のままです。

Windows ではタスクトレイ、macOS ではメニューバーのメニューから開く・取得・終了を操作できます。`Ctrl+Q`（macOS は `⌘Q`）で終了するときは、取得中の通信を取り消し、データベースへの書き込みが済むのを待ちます。

画面を出さずに 1 回だけ取得する場合は、次のように実行します。

```powershell
$app = Join-Path $env:LOCALAPPDATA 'Programs\Game News Nexus\game-news-nexus.exe'
& $app --fetch-once
```

常駐中のアプリを終了する場合:

```powershell
& $app --quit
```

`--fetch-once` は常駐中も使えます。購読の保存などと重なってロックを取れなかった場合は、購読を変更せずにエラーで終了します。少し待ってから実行し直してください。

キーボード操作・予定の読み取り・画像の扱いと制約は [使い方の詳細](docs/usage.md)、引数と終了コードは [コマンドライン](docs/cli-reference.md)を参照してください。

## 設定

ゲームの選択や取得間隔はアプリの画面で設定できます。カタログにないゲームは、取得方式を選んで手元の定義を作り、「テスト取得」で確認します。カタログを自分用にコピーして調整することもできます。

| 内容 | Windows | macOS |
| --- | --- | --- |
| 設定・購読 | `%APPDATA%\game-news-nexus\config\` | `~/Library/Application Support/game-news-nexus/` |
| 記事・ログ | `%APPDATA%\game-news-nexus\data\` | `~/Library/Application Support/game-news-nexus/` |
| 画像キャッシュ | `%LOCALAPPDATA%\game-news-nexus\cache\images\` | `~/Library/Caches/game-news-nexus/images/` |

`GNN_HOME` を指定すると、データをそのディレクトリの下にまとめられます。定義の書き方・秘密の値・取得先の制限は [設定の詳細](docs/configuration.md)を参照してください。

## 開発

ツールとタスクは [mise](https://mise.jdx.dev) で管理しています。ビルドには Windows の MSVC 環境、または macOS の Xcode Command Line Tools が必要です。

```bash
mise install
mise run build
```

```bash
mise run lint
mise run test
```

| コマンド | 内容 |
| --- | --- |
| `mise run run` | アプリを起動 |
| `mise run catalog-check` | 同梱カタログを検査 |
| `mise run icons` | アプリアイコンを書き出す |
| `mise run licenses -- --check` | 第三者のライセンスの一覧を確認 |

ビルドするタスクは、必要な辞書を先に用意して内容を照合します。構成・取得定義の検査・配布物の作り方は [開発の詳細](docs/development.md)、購読更新の測定条件と生データは [ベンチマーク](docs/benchmarks/subscriptions.md)を参照してください。リリースは GitHub Actions の「Release」から実行します。

## ライセンス

[MIT License](LICENSE)。配布物には、組み込んだ Rust のクレートと辞書のライセンスも含みます。[第三者のライセンス](crates/gnn-app/assets/licenses/README.md)を参照してください。
