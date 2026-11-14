# 開発

## ソースから動かす

ツールのバージョンは [mise](https://mise.jdx.dev) で固定しています。

```bash
mise install
```

```bash
mise run run
```

ビルドするタスク (`build`・`run`・`test`・`lint`・`release`・`fetch-once`) は、先に予定の読み取りに使う辞書 (hasami のリリースの IPAdic、約 15 MB) を `crates/gnn-schedule/dict/` に取得します (`mise run dict`)。 `crates/gnn-schedule/dict/bundled.json` に固定した大きさと SHA-256 で照らし、2 回目からは照合だけです。 `cargo` を直接使うときは、先に `mise run dict` を実行してください。

リリース版のビルド:

```bash
mise run release
```

`target/release/game-news-nexus.exe` (macOS は `target/release/game-news-nexus`) ができます。辞書も埋め込むので、 1 つの実行ファイルで動きます。インストーラーと DMG の作り方は「開発」の「配布物を作る」にあります。

## 構成と検証

```text
crates/
├── gnn-core/      ドメインモデル・取得の定義のスキーマ・カタログ・購読・SQLite ストア・予定の型と表
│   │              (通信と UI に依存しない)
│   └── catalog/   game-news-nexus-config の写し。ビルド時に埋め込む
├── gnn-fetch/     取得エンジン: HTTP (取得先の制限)・定義の解釈・本文の整形・日時・画像キャッシュ・カタログの取り込み
├── gnn-schedule/  予定の読み取り (hasami の形態素解析。辞書 IPAdic を埋め込む)
│   └── dict/      固定した辞書のリリース・大きさ・SHA-256 (bundled.json)。辞書は mise run dict で取る
├── gnn-cli/       gnn コマンド: カタログの検査・実サイトでの試行・JSON Schema の書き出し
└── gnn-app/       gpui のアプリ: 画面・常駐の定期取得・トレイ (macOS はメニューバーと Dock)・通知・二重起動の防止と --quit
    └── assets/
        ├── icons/     アプリアイコン (元の絵は SVG。`mise run icons` で .ico・.icns・PNG を書き出す)
        └── licenses/  配布物に入れる第三者のライセンス (Rust のクレートの一覧 THIRD_PARTY_LICENSES.md・IPAdic の辞書など)
tools/
├── dict/          辞書の取得と照合 (gnn-dict。mise run dict)
├── icons/         アイコンの書き出し (gnn-icons)
└── licenses/      クレートのライセンスの一覧を cargo-about で作って確かめる (gnn-licenses。mise run licenses)
packaging/         配布物のひな形 (Windows は Inno Setup のスクリプト、macOS は Info.plist)
└── licenses/      クレートのライセンスの一覧の設定 (about.toml・allowlist.toml) とひな形 (about.hbs)
scripts/           配布物を作る・確かめるスクリプト
.github/workflows/ CI (ci.yml) とリリース (release.yml)
```

```bash
mise run test
```

```bash
mise run lint
```

同梱のカタログを検査する・実サイトで試す:

```bash
mise run catalog-check
```

```bash
mise run catalog-try
```

`gnn` コマンドは、カタログのリポジトリの CI でも使います。

```bash
cargo install --locked --git https://github.com/owayo/game-news-nexus gnn-cli
```

| コマンド | 説明 |
| --- | --- |
| `gnn check <カタログ>` | 書式・決まり・CSS セレクタ・JSON Schema の古さを検査する |
| `gnn try <カタログ> [ID ...] [--adult-consent]` | 実サイトから取得し、段ごとの件数と記事の先頭を出す。成人向けのソース (`adult: true`) は `--adult-consent` (18 歳以上で、成人向けの内容を取得することに同意する場合だけ付ける) のときだけ試し、GitHub Actions の中では記事の題名と本文を出さない |
| `gnn schema [--out <ファイル>]` | 取得の定義の JSON Schema を書き出す |
| `gnn convert <sources.toml> <出力先>` | 書式 1 (TOML) の定義を YAML に変換する |

`gnn-cli` は辞書 (gnn-schedule) に依存しないので、`mise run dict` をしなくてもビルドできます。

### 配布物を作る

Windows のインストーラー (Inno Setup) と macOS の DMG は、リリースビルドの実行ファイルから作ります。出力は `target/dist/` です。Windows の実行ファイルは C ランタイムを静的にリンクします (`.cargo/config.toml`。 既定の MSVC のビルドは VCRUNTIME140.dll を使い、入っていない PC で起動できません)。

Windows (Inno Setup 6.3 以降が要ります。`winget install --id JRSoftware.InnoSetup -e`):

```powershell
mise run dict
mise run release
mise run installer
```

macOS (Apple Silicon。ad-hoc で署名し、公証はしません。`mise run bundle` は .app だけを作ります):

```bash
mise run dict
mise run release
mise run dmg
```

`cargo build --release --target <ターゲット>` で作ったときは `mise run installer -- -Target <ターゲット>`・ `mise run dmg -- --target <ターゲット>` のように渡します。

| ファイル | 中身 |
| --- | --- |
| `packaging/windows/game-news-nexus.iss` | Inno Setup のスクリプト。利用者ごとに入れ、アプリと同じ名前・AppUserModelID のショートカットを作る |
| `packaging/macos/Info.plist` | .app の Info.plist のひな形 (bundle id は `io.github.owayo.game-news-nexus`。実行ファイルには埋め込まない) |
| `scripts/build-installer.ps1` | インストーラーを作る。ショートカットの ID などがアプリのコードと同じかも照らす |
| `scripts/bundle-macos.sh` | .app を作って署名し、DMG にする |
| `scripts/test-installer-windows.ps1` | インストーラーを入れて・更新して・消して確かめる。起動中のアプリを `--quit` で終わらせることと、終わらないときに今までの方法で閉じることも確かめる (CI 専用。スタートメニューとレジストリを書き換える) |
| `scripts/test-installer-fixture.cs` | インストーラーの検査でアプリの代わりに入れる小さな実行ファイル (C#。.NET Framework の csc.exe で作る)。`--quit` に応じる動きと応じない動きを持つ |
| `scripts/test-bundle-macos.sh` | .app と DMG の作り方を、アプリの代わりの小さな実行ファイルで確かめる (`mise run test-bundle`) |

### 第三者のライセンスの一覧

配布物には `LICENSE` と `crates/gnn-app/assets/licenses/` をそのまま入れます (Windows は入れた場所の `licenses`、 macOS は `Game News Nexus.app/Contents/Resources/licenses`)。実行ファイルに組み込んだ Rust のクレート (間接の依存を含む約 550 個) のライセンスの一覧 `THIRD_PARTY_LICENSES.md` は [cargo-about](https://github.com/EmbarkStudios/cargo-about) で作ってコミットしてあり、依存か `packaging/licenses/` を変えたら作り直します (古いままだと CI が止まります)。

```bash
mise run licenses
```

`mise run licenses -- --check` は作り直さずに、古くないかだけを確かめます (CI が実行します)。作るときに次のどれかに 当たると、全部を出して止まります。

- `packaging/licenses/about.toml` の `accepted` にないライセンスのクレートが入った
- cargo-about が警告を出した (`clarify` に書いた本文のファイルの SHA-256 が、クレートを上げて変わった など)
- クレートの本文が見つからず SPDX の標準の文面に置き換わったのに、理由が `packaging/licenses/allowlist.toml` にない
- 一覧に載せるライセンスが、クレートの Cargo.toml のライセンスの式を満たさない (`clarify` が古い)
- クレートの中の NOTICE や、下の階層のライセンスのファイル (内包したコードのものなど) が一覧に入らず、理由もない

| ファイル | 中身 |
| --- | --- |
| `packaging/licenses/about.toml` | cargo-about の設定。使ってよいライセンスとその優先順・対象 (gnn-app と配る 2 つのターゲット)・本文を拾えないクレートの本文のファイル (`clarify`) |
| `packaging/licenses/allowlist.toml` | 原文を取得できず標準の文面を載せるクレートと、一覧に入れないライセンスのファイル (どちらも理由付き) |
| `packaging/licenses/about.hbs` | 一覧のひな形 (Handlebars) |
| `tools/licenses/` | cargo-about を呼んで結果を確かめ、ひな形に当てはめる (gnn-licenses) |

### CI とリリース

- CI (`.github/workflows/ci.yml`) は Windows と macOS で、書式と Clippy (`mise run lint`)・テスト・カタログの検査と、 配布物に入れるクレートのライセンスの一覧が古くないか (`mise run licenses -- --check`) を回し、辞書がなくても `gnn-cli` を ビルドできることと、インストーラーと DMG の作り方 (アプリの代わりで、入れる・起動中に更新する・消すまで。起動中のアプリは `--quit` で終わらせ、終わらないときは閉じる) を確かめます。ワークフローとシェルスクリプトは `mise run lint-scripts` (actionlint・shellcheck) で検査します。
- リリースは、Actions の「Release」を `main` で実行します (`dry_run` にすると版の計算だけ)。CI が通ったコミットからだけ出し、 版を日本時間の `yy.m.連番` (連番は月ごとに 100 から) で決めて `Cargo.toml` と `Cargo.lock` に書き、タグを付けます。 Windows のインストーラー・macOS の DMG・`SHA256SUMS` を Release に添付し、公開した添付を照らします。

依存とツールの更新は [depup](https://github.com/owayo/depup) で行います。依存を上げたら、クレートのライセンスの一覧も 作り直します (`mise run licenses`)。mise のタスクごとに固定したツール (`lint-scripts` の actionlint・shellcheck と `licenses` の cargo-about) は depup が上げないので、公開から 14 日以上たった版に手で上げます。

```bash
depup
```
