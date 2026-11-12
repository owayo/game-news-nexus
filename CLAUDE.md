# CLAUDE.md

Game News Nexus: ゲームのお知らせを集めて読む Windows と macOS (Apple Silicon) 向けの常駐型デスクトップアプリ (Rust + gpui-kit)。
利用者向けの説明は README.md。ゲームごとの取得の定義は別リポジトリ
[game-news-nexus-config](https://github.com/owayo/game-news-nexus-config) (カタログ) にあり、
`crates/gnn-core/catalog/` はその写し (ビルド時に埋め込む同梱版)。

## コマンド

ツールは mise で固定している (`mise.toml`)。タスクも mise に定義してある。

| 目的 | コマンド |
| --- | --- |
| 辞書を用意する (ビルドの前に要る) | `mise run dict` (hasami のリリースから `crates/gnn-schedule/dict/ipadic.hsd` を取り、`bundled.json` の大きさと SHA-256 で照らす。2 回目からは照合だけ) |
| ビルド | `mise run build` (`cargo build --workspace`) |
| 起動 | `mise run run` |
| テスト | `mise run test` |
| 書式と Lint | `mise run lint` (`cargo fmt --check` と `cargo clippy -D warnings`) |
| 画面なしで 1 回取得 | `mise run fetch-once` |
| 同梱のカタログを検査 | `mise run catalog-check` (`gnn check crates/gnn-core/catalog`) |
| 同梱のカタログを実サイトで試す | `mise run catalog-try` (`gnn try crates/gnn-core/catalog [ID ...]`) |
| 取得の定義の JSON Schema を作り直す | `mise run schema` |
| アプリアイコンを書き出す | `mise run icons` (`crates/gnn-app/assets/icons/*.svg` から .ico・.icns・PNG) |
| 配布物に入れるクレートのライセンスの一覧を作り直す | `mise run licenses` (cargo-about。`crates/gnn-app/assets/licenses/THIRD_PARTY_LICENSES.md`)。`mise run licenses -- --check` は古くないかだけを確かめる (CI) |
| 配布物を作る | `mise run installer` (Windows。Inno Setup 6.3 以降)・`mise run dmg` (macOS。`mise run bundle` は .app だけ)。先に `mise run dict` と `mise run release`。出力は `target/dist` |
| 配布物の作り方を確かめる | `mise run test-bundle` (macOS)。`mise run test-installer` は CI 専用 (利用者のスタートメニューとレジストリを書き換える) |
| ワークフローとシェルスクリプトの検査 | `mise run lint-scripts` (actionlint・shellcheck) |
| 依存とツールの更新 | `depup` (Rust と mise の両方)。`depup --mise` は mise だけ。タスクごとに固定したツール (`lint-scripts` の actionlint・shellcheck、`licenses` の cargo-about) は depup が上げないので手で上げる (公開から 14 日以上たった版)。依存を上げたら `mise run licenses` |

手元で動かして確かめるときは、利用者のデータを汚さないよう `GNN_HOME` に作業用ディレクトリを指定する。
画面を操作せずに確かめるための開発用の環境変数 (デバッグビルドだけで効く):
`GNN_DEV_PAGE` (news・releases・sources・settings)、`GNN_DEV_SOURCE` (ソース画面で選ぶ ID)、
`GNN_DEV_TEST` (選んだカタログのソースをテスト取得)、`GNN_DEV_TAB=yaml` (編集画面を YAML タブで開く)、
`GNN_DEV_CROP_IMAGE` (アイコンの切り抜き画面を開く)。

## 構成と境界

- `crates/gnn-core`: 通信と UI に依存させない。
  - 取得の定義のスキーマ (`source.rs`、書式 2 の YAML)、旧書式 1 (TOML) と変換 (`source_v1.rs`)、
    厳格な YAML の読み書き (`yaml.rs`)
  - カタログ (`catalog.rs`: ファイルごとの取り込みと前の版の維持、カタログの決まり、購読との解決)、
    購読 (`subscription.rs`)、手元のネットワークの判定 (`net.rs`)
  - 設定ファイル (`config.rs`: 置き場所、手元の定義、カタログの控え、書式 1 からの移行、
    一時ファイルに書いてから置き換える `write_atomic`)、
    SQLite ストア (`store.rs`)、ドメインモデル (`entry.rs`)、旧形式 JSONL (`legacy.rs`)、
    新しいソースのひな形 (`templates.rs`)、同梱カタログの埋め込み (`build.rs`)
  - 画像の同一性 (`image_ref.rs`: `url_identity.images` に合う画像の参照の作り方、本文の書き換えと戻し方)
  - 予定の型 (`schedule.rs`、gnn-schedule と共有)、予定の表と表示の状態 (`agenda.rs`: 突き合わせた
    あとの行、今の時刻でのバッジ・今日明日の並び・サイドバーの印・本文の取り直しの判断・旧形式の日付)
- `crates/gnn-fetch`: 取得エンジン (`engine.rs`: prepare → lists (ページ送り) → まとめ → detail → body)、
  テンプレート (`template.rs`)、HTTP と取得先の制限 (`http.rs`)、カタログの取り込み (`catalog.rs`)、
  本文整形 (`clean.rs`)、日時 (`dates.rs`)、画像キャッシュ (`image_cache.rs`: 画像と横の `.meta` (検証子と、書いた
  長さと SHA-256)、照らして読む `read_checked`、条件付きの取得 `fetch_image`)。保存と UI に依存させない。取得結果と
  診断を返すだけで、保存は呼び出し側が行う。
- `crates/gnn-schedule`: 予定の読み取り。hasami の形態素解析と、埋め込んだ IPAdic (`dict/ipadic.hsd` を
  `include_hsd!` で埋め込む) で題名と本文を読み、`gnn_core::schedule::Schedule` を返す (`Extractor::extract`)。
  gnn-core にだけ依存させ、通信・保存・UI に依存させず、今の時刻も読まない (同じ記事と文脈からは同じ結果)。
  段は `document` (HTML の構造・追記の区画・原文への対応) → `temporal` (日時の字句。年は決めない) →
  `clause`・`reading` (形態素から節の対象・動作・否定・予定/条件/推量・修飾節) → `facts` (日時の役割の結び付け) →
  `resolve` (年・相対の日付・地域時刻) → `merge` (記事の中の統合)。記事どうしの突き合わせは `reconcile`
  (日付の索引で引く)。`build.rs` は辞書の大きさと SHA-256 を `dict/bundled.json` と照らすだけで、通信しない。
- `crates/gnn-cli`: `gnn` コマンド (check・try・schema・convert)。カタログのリポジトリの CI も使う。
- `crates/gnn-app`: gpui-kit のアプリ。`services.rs` が tokio ランタイム・ストア・定期取得・
  カタログの取り込み・予定の読み取りの待ち行列・画像の先読みと確かめ直し・画像の取り直しを持ち、画面とは
  `ServiceEvent` (flume) でやり取りする。画面の画像は `http_client.rs` (キャッシュを照らして先に見て、画像の参照は
  ストアの取得 URL で取る。gpui が覚えている失敗を消して読み直させる。`Workspace` の根に置く `WatchedImages` で
  gpui が読めなかった画像を見つける) を通る。画像の取り直しの頼みのまとめ方と上限は `image_recovery.rs` (通信も
  保存もしない待ち行列)。
  画面の共有状態は `state.rs` の `AppState` (予定の表と、状態が変わる時刻を待つ時計も持つ)。
  OS に関わる処理は `autostart.rs` (Windows は Run キー、macOS は `~/Library/LaunchAgents` の plist)・
  `shortcut.rs` (Windows の通知のためのスタートメニューのショートカット)・`tray.rs` (トレイ、macOS は
  メニューバー)・`instance.rs` (ロックファイル + localhost の TCP。2 つ目の起動の `show` と `--quit` の `quit`。
  両 OS 共通)・`macos.rs` (Dock の出し入れ・アプリのメニュー・⌘H など) に置く。
- `tools/dict`: 辞書の取得と照合 (gnn-dict。hasami の `download` を使う)。
- `tools/icons`: アイコンの書き出し (resvg)。元の絵は `crates/gnn-app/assets/icons/app.svg` と
  `tray-template.svg` で、書き出したファイルもコミットする。
- `tools/licenses`: 配布物に入れるクレートのライセンスの一覧を作る (gnn-licenses。`about.rs` が cargo-about と cargo を
  呼んでログを読み、`checks.rs` が結果を確かめ、`render.rs` がひな形に当てはめる)。設定とひな形は `packaging/licenses/`。
- `packaging/`・`scripts/`: 配布物。Windows は Inno Setup のインストーラー (`packaging/windows/game-news-nexus.iss`・
  `scripts/build-installer.ps1`)、macOS は ad-hoc 署名の .app と DMG (`packaging/macos/Info.plist`・
  `scripts/bundle-macos.sh`)。CI は `.github/workflows/ci.yml`、リリースは `release.yml` (Actions から手で実行する)。

## 守ること

- **gpui-kit の API は推測で書かない。** 依存は `gpui-kit` だけ。シグネチャは
  `~/.cargo/registry/src/*/gpui-pre-*`・`gpui-base-*`・`gpui-component-*` のソースか、
  gpui-kit リポジトリの `skills/gpui-kit` と `examples` で確かめる。
- アイコンは `gpui_kit::assets::IconName` (Lucide 全種) を使う。名前は
  `gpui-kit-assets-*/assets/icons/*.svg` で確かめる (`file-warning` などは改名されている)。
  `gpui_kit::component::IconName` は一部しか持たない。
- `font_semibold()` などは `gpui_kit::component::StyledExt` にある。
- ストア (`Services::with_store`) は UI スレッドで触らない。画面からは `cx.background_executor().spawn`、
  サービスからは `Services::blocking` を通す。
- 終わるときは `Services::shutdown` (`Cleanup` から。`--fetch-once` は取得と予定の読み取りを済ませたあと) が、
  止める合図 (`stop`) を立てて新しい処理を始めず、取得の通信を取り消し、始まっているストアへの書き込みが済むのを
  待ってストアを閉じる (閉じるまで含めて長くても `SHUTDOWN_TIMEOUT`。そのあとのログの書き切りは 1 秒ほどまで)。
  そのため services の runtime の上の処理は `Services::spawn` で始め (受け付けと合図を `admission` で並べ、
  `TaskTracker` に数える)、ループ・通信・順番待ちは合図で抜ける (`Services::unless_stopped`。合図と同時に済んでも
  合図を先に見る)。`runtime.spawn`・`tokio::task::spawn_blocking` を直接使うのは、終わるときに待たないもの (切れても
  害がない画像キャッシュの掃除 (prune と、gpui が読めなかった画像を捨てる `CachingHttpClient::decode_failed`)、合図で
  取り消す画面の画像の取得と読み直しの予約 (`http_client.rs`)、`shutdown` 自身の待つ係) だけ。画像の取り直しの
  ループ (`image_recovery_loop`) も `Services::spawn` で始め、合図で抜ける (取り直しの途中なら取り消す)。画面の
  HTTP クライアントは、合図のあとは取り直しも自動の読み直しも頼まず、gpui が読めなかった画像も捨てない。
- サービスからのストアの処理 (`blocking`) は、ロックを取ったあとで合図を見て、合図のあとなら `Stopped` を返して
  始めない (誤りとして記録しない)。合図の前に始めた書き込みは最後まで行う。取り消した取得は保存も取得ログの記録も
  せず (前回の取得の時刻のままなので、次の取得で取り直す)、画面に終わりを知らせない。閉じたあとの `with_store` は
  誤りを返す。競合の順番を決めるテストは、テストだけで効く止め場所 (`services.rs` の `Pause`) を使い、時間で待たない。
- 同じ見た目を複数の画面で作るときは `ui/components.rs` に部品を置いて使う。
- 日時はタイムゾーンを捨てない。タイムゾーンのない日時はソースの `timezone` (既定 Asia/Tokyo) で読み、
  0 時 0 分 0 秒は日付だけの投稿 (`TimePrecision::Date`) とする。
- 記事の同一性は `(source_id, external_id)`。`external_id` は `id` → `url` → `title` の順に決まる。
  本文の差分で新着扱いにしない。一覧の署名 (`list_signature`。詳細を取り直すかの判断に使う) だけが
  変わったときは、署名を残すだけで更新に数えない。
- 画像の URL の問い合わせは推測で外さない (署名付きの URL は外すと取れず、`?v=` や日付の `?t=` は中身が
  変わったときの版)。取得のたびに変わる問い合わせ (署名・期限・取得した時刻のキャッシュ破り) は、取得の
  定義の `url_identity.images` (`origin`・`path_prefix`・`ignore_query`) で宣言する。宣言に合う画像は、
  保存のとき (`Store::apply_fetch`) に本文と `image_url` を参照 `https://source-image.invalid/<ソース ID>/<鍵>`
  (`gnn_core::image_ref`) にし、配信元の URL (取得 URL) は記事ごとに `entry_images` に記録する。記事の比較・
  一覧の署名・画像キャッシュの鍵は参照で決め、取りに行くときは記録した取得 URL を使う。参照のまま
  (`.invalid` のホストへ) 外へ取りに行かない。保存済みの値は、記録した取得 URL に戻してから今の決まりで
  直して比べる (宣言を足す・変えるときに、版も既読も変えずに移す)。下書き (`EntryDraft`) は配信元の URL のまま。
- 画像キャッシュの検証子 (ETag・Last-Modified) は DB ではなく画像の横 (`<鍵>.meta`) に置き、画像と一緒に消す。
  画像のファイルの mtime は「最後に配信元で確かめた時刻」(取ってきた・304・同じバイト列の 200 で今にする。読んだ
  だけでは進めない。prune もこれで並べる)。画像を入れ直すときは「meta を消す → 画像 → meta」の順。取りに行く前の
  世代 (`ImageCache::generation`) のあとで入れ直されていたら、遅れて届いた結果で上書き・確認しない (`store_if`)。
  画像を取る処理は `gnn_fetch::image_cache::fetch_image` に寄せる (先読み・確かめ直し・画面で同じクライアントと
  条件)。HTML など画像でない応答はキャッシュに入れない。
- `.meta` には、書いたときの長さと SHA-256 (`Integrity`) も入れる (検証子がなくても書く。前の版の `.meta` は検証子
  だけ)。読むときは `ImageCache::read_checked` で照らす: `Intact` (書いたときのまま)・`Unverified` (照らすものがない =
  これを入れる前の版で書いた画像・`.meta` がない・読めない・形が違う。壊れたとはしない)・`Corrupted` (合わない)・
  `Missing`。ふだんはロックを取らずに読み、読む前後で世代が変わったか合わなければ世代のロックの中で読み直す (画面の
  読み込みを 1 つのロックで並ばせない)。条件付きの取得は `Intact` のときだけで、`Unverified`・`Corrupted` は条件なしで
  取って比べる (304 で延命しない。同じバイト列なら `.meta` を書いて `Intact` にする)。304 のあとも integrity を残す。
  先読みの選び方 (`select_prefetch`) と prune は中身を読まない。
- 画面の HTTP クライアントは、キャッシュを `read_checked` で読む。`Corrupted` は gpui に渡さず `discard_corrupted` で
  捨てて、キャッシュにないときと同じ道で取り直す (旧形式 `https://legacy-image.invalid/...` は取り直せないので捨てずに
  誤りにする)。gpui に返したバイト列の世代と SHA-256 と照らせたか (`Served`) を、今の読み込みの番号で覚える (古い
  `send` が遅れて終わっても書き換えない。取ってきた画像もキャッシュから読み直して照らしたもので覚える)。gpui が読め
  なかった画像は、`Workspace` の根に `image_cache(..)` で置いた `WatchedImages` (`ImageCache::load` で
  `window.use_asset::<ImgResourceLoader>` の結果を見る。読み込みと結果の覚え方は gpui の既定のまま) が見つけ、
  `CachingHttpClient::decode_failed` を呼ぶ (描画の中なのでディスクは触らない)。返したのが `Unverified` なら、鍵ごとに
  起動中 1 回、`discard_unverified` (読んだときの世代と SHA-256 のままなら消す。先読みが同じバイト列を確かめて
  `Intact` にしていれば消さない) で捨て、`ServiceEvent::ImagesDiscarded` で読み直させる (もう消えていても読み直す)。
  `Intact` なのに読めない画像は配信元のバイト列が読めないので取り直さない。ほかのプロセス (`--fetch-once`) と同時に
  入れ直した画像は、まれに捨てて取り直すことがある (プロセスをまたぐロックはない)。
- 画像キャッシュのファイル (画像と `.meta`) は、書き込みごとに名前の違う書きかけ (`<名前>.<PID>.<通し番号>.tmp`)
  に書いて閉じてから置き換える (`ImageCache::replace`)。PID だけの名前にすると、同じ画像を同時に書く処理
  (先読みと画面) が 1 つの書きかけを分け合い、中身が混ざる。書くのにも置き換えるのにも失敗したら書きかけを消す。
  Windows は、置き換える先をほかのプログラム (ウイルス対策の検査など) が開いていると置き換えを断ることがある
  (`ERROR_ACCESS_DENIED`) が、前の画像が残るだけなので待ってやり直さない (呼び出し元は async の中で同期のまま呼ぶ)。
  書き込みの途中で落ちたり終了で切れたりしても書きかけが残るだけで、`prune` が 1 時間たったものを消す。
- 取得のあとの先読み (`Services::prefetch_images`) は、キャッシュにない画像を先に取り、確かめてから 24 時間たった
  画像 (参照も普通の URL も) を古い順に 32 枚・64 MB まで条件付きで確かめ直す。失敗した確かめ直しはメモリで
  待たせる (`ImageFailure`)。入れた・入れ直した鍵は `ServiceEvent::ImagesCached { available, changed }` で画面へ送る。
- 画面の HTTP クライアントは、参照を記録した取得 URL で取って 401・403・404・410 なら `ReferenceImages::request_recovery`
  でソースの取り直しを頼む (取れなかった URL を添える。通信の失敗・5xx・429 は頼まず、30 秒後・2 分後に読み直す。
  予約には番号を付け、記事を開き直す前の予約は使わない。Retry-After が 10 分より長ければ自動では読み直さない)。
  取り直しは `image_recovery::RecoveryQueue` (2 秒まとめる・画像ごとに 30 分あけて起動中 3 回・ソースごとに完了から
  60 秒・1 時間に 3 回・取得中と取得の失敗の待ち時間は待つ・新しい URL でも取れない画像と新しい URL が見つからない
  画像は `give_up`・新しい URL でも 4xx の画像が 2 件ならソースを 1 時間休む・一覧や詳細の一時的な失敗は
  `retry_later`) を通す。取り直す前に、記録した取得 URL が取れなかった URL と違えばそれで取る。詳細のあるソースは
  画像を持つ記事 (`Store::image_holders`) を `RunInput::recheck_ids` に入れ (上限を超える画像は次の回に回す)、
  記事ごとの取れなかった理由 (`RunOutput::recheck_misses`) で「消えた」と「一時的な失敗」を分ける。
  gpui の画像は `App::remove_asset::<ImgResourceLoader>` で消してから `refresh_windows` で読み直させる。`fetch_asset` は
  gpui が覚えていない URI に呼ばない (読み込みを始めてしまう)。中身が変わった画像は `drop_image` でテクスチャも外す。
  gpui は画面に渡した URI の文字列でアセットを覚え、HTTP クライアントが受け取る URI にはフラグメントがないので、
  画面に渡す画像の URI は `http_client::display_uri`・`display_html` でフラグメントを外す。
- `RunInput::recheck_ids` の記事は、取得する期間 (`max_age_days`) と件数の上限 (`max_items`) によらず読み、一覧に
  見つからなければ保存済みの値 (`KnownItem`) から詳細を取る (一覧を置き換えるソースを除く。その詳細の失敗はソースの
  失敗にしない)。取り直せなかった記事は `RunOutput::recheck_misses` (`NotFound`・`Gone`・`Failed`) で返す。HTTP の
  状態コードは `gnn_fetch::http::http_status` で誤りから取り出す。
- **カタログの定義は第三者が書く前提で扱う。** 秘密の値 (`{{secret:...}}`) は使わせず
  (`RunInput::allow_secrets`)、取得は `NetworkPolicy::PublicOnly` で行う。手元の定義も既定は
  `PublicOnly` で、利用者が許したときだけ `Any`。本文の画像は常に `PublicOnly`。
- 処理の流れは段 (`prepare → lists → detail → body`) で固定する。任意の処理を並べるステップ列や
  スクリプトは入れない。柔軟さは段の中の機能で足す。
- 書式 2 に機能を足したら、`source.rs` の `LATER_FEATURES` に「使っているかの判定」と「入れたアプリの版」を
  加え、`mise run schema` で JSON Schema を作り直す。カタログの `min_app_version` はこの版以上が要る。
  未知の項目は読まない (`deny_unknown_fields`) ので、古いアプリはそのソースの前の版を使い続ける。
- 同梱のカタログ (`crates/gnn-core/catalog/`) と game-news-nexus-config は同じ中身にする。定義を変えたら
  両方に入れ、`mise run catalog-check` と `mise run catalog-try` (実サイト) で確かめる。
  公開したソースの `id` は変えない (記事と購読が引き継がれなくなる)。
- YAML を書き出すときは `gnn_core::yaml::to_string` を使う (文字列を単一引用符で囲み、正規表現を
  そのまま書ける)。`serde-saphyr` の書き出しをそのまま使うと、`\` を含む文字列が二重引用符になる。
- 設定・購読・手元の定義・カタログの控え・旧形式の `game_info.jsonl` は `config::write_atomic` で書く。書き込みごとに
  別の一時ファイル (`.<名前>.<PID>.<通し番号>.tmp`。`create_new` で作る) に書いて sync し、閉じてから置き換える
  (一時ファイルの名前を PID だけで決めない。同じプロセスで同じファイルを同時に書くと一時ファイルを共有する)。
  失敗したら一時ファイルを消し、置き換えたあと、同じファイルの一時ファイルのうち最終更新から 1 時間たったもの
  (落ちたときの残り) を消す (ほかの名前のファイルには触らない。書き出し先は利用者のディレクトリ)。同時に書いたときに
  残るのは成功したどれか 1 つで、順番は約束しない (決めたいときは呼び出し側で並べる。今の呼び出し元は画面のスレッド・
  `shared` のロック・`catalog_lock`・ストアのロックで並んでいる)。Windows が置き換えを断ったとき (`ERROR_ACCESS_DENIED`)
  は待ってやり直さない (呼び出し元は画面のスレッドやロックの中で同期のまま呼ぶ)。電源断のあとに新しい中身が残る
  ことまでは約束しない (ディレクトリは sync しない)。Windows では、開かれているファイルの置き換え (std の rename が
  `MoveFileExW` に断られてやり直す POSIX の置き換え) と重なって、ほかのスレッド・プロセスがその名前で開くと
  見つからない (`NotFound`) ことがあるのを実験で見ている (置き換えが戻った直後に同じスレッドで開く実験では起きな
  かった)。書き込みと重なりうる読み手は、見つからないことを「ない」と決めつけない (今の読み手は、同じプロセスの
  中では書き込みと重ならない)。
- 画面の文言とコメントは日本語。README とこのファイルは変更に合わせて更新する。
  カタログの書き方を変えたら game-news-nexus-config の CONTRIBUTING.md も直す。
- コミットは Conventional Commits 形式で、説明 (body) も詳しく書く。

### 予定の読み取り

- 予定は記事から作り直せる派生データ。読み取り結果 (`schedule_analysis`) は記事の版 (`entries.revision`。
  題名・本文・投稿日時が変わったら上がる)・抽出器の識別子 (`gnn_schedule::extractor_id`)・文脈の指紋
  (本文の地域時刻) と一緒に保存し、どれかが違えば読み直す。「未解析」(行なし)・「該当なし」・「失敗」を分ける。
- 読み取りは services の待ち行列で、ストアのロックを外して `spawn_blocking` の中で行う (抽出器はスレッドごとに
  1 つ)。保存は `Store::save_analyses` で記事の版を照合し、既読・`updated_at`・`sort_at`・新着の扱いを変えない。
  予定の表 (`schedule_items`) はソースごとに `gnn_schedule::reconcile` の結果から同じトランザクションで作り直し、
  記事の中身が変わる・消えるとその記事の行を外してソースを作り直し待ち (`schedule_stale_sources`) にする。
- 「メンテ中か」「今日・明日か」は保存せず、`gnn_core::agenda` で今の時刻と表示の地域時刻から計算する
  (時間帯は開始を含み終了を含まない、完了の報告がなければ予定のまま、終了予定を過ぎても完了にしない、日付だけから
  「メンテ中」にしない、終了未定を無期限にしない、所要時間からの終わりは「頃」)。規則を変えたら agenda.rs の
  テストを足す。
- engine にメンテナンスの意味を持ち込まない。本文の取り直しは services が予定の表から決めた外部 ID を
  `RunInput::recheck_ids` で渡す (期限は `agenda::needs_recheck`)。
- 取得の定義の `timezone` は投稿日時、`schedule.timezone` は本文の日時、`schedule.server_timezone` は
  「サーバー時間」の地域時刻。`schedule` を変えても取り直さず、保存済みの記事を読み直す。
- データベースの形を変えたら `store.rs` の `DATABASE_VERSION` (SQLite の `user_version`) を上げ、`migrate`
  (列の有無で足し引きする) と、前の版のデータ入りの移行テストを足す。取得の定義の書式の版 (`source::SCHEMA_VERSION`)
  とは別物。
- `--fetch-once` は取得のあと予定の読み取りもすべて済ませる。
- gnn-schedule の規則 (語の一覧・構文の形・統合の決まり) を変えたら `RULES_VERSION` を上げる (保存済みの結果を
  起動時に読み直す)。
- gnn-schedule のテストは埋め込んだ実際の辞書で行う。作文の例は実際の記事の文を写さず、書き方をまねて作る。
- 予定の型の意味 (`gnn_core::schedule`):
  - `Maintenance::planned`・`Period::window` は、すべての延長・日程変更を反映した最後の時間帯。変更の前と後は
    `Change` に持つ (`Extended { from, end }`・`Rescheduled { from, to }`)。
  - 1 件のメンテナンスの対象 (`Scope::Named`) は 1 つ。複数の対象は対象ごとの件に分け、まとめるのは画面だけ。
  - 「実施中」の報告は `Maintenance::ongoing` (`as_of` はいつの時点の報告か)。開始の時刻に入れない。抽出器は、
    「実施中」の記述 (実施しております・継続・延長・終了していません・現在の〜メンテナンス) のうち、いちばん新しい
    区画のものを採る。`as_of` は追記の印の日時 (時刻があれば `Instant`、日付だけなら `When::Date`、どちらも
    `Explicit`)、なければ投稿日時、なければ `first_seen_at` (どちらも `Inferred`)。結果が `Scheduled` で、時点が
    開始より後のときだけ付ける。
  - `Extended::from` は、書かれた始まり → 延長より前の記述の予定の終わり (`Inferred`) → 前の延長の終わり →
    `Unspecified` の順に決める。延長は区画ごとに 1 つ。`Rescheduled` は変更の前を古い順に並べ、`to` は次の変更の前
    (最後は今の予定)。
  - 突き合わせ (`reconcile`) は、名前のない報告をどの対象の件にも、名前のある報告を同じ名前の件にだけ当てる。
    `Resolved::ongoing` は、予告と結び付けた報告のうち `as_of` がいちばん新しいもの。
- 実施中の報告からは、結果が予定どおりで開始の時刻がないときだけ、時刻まで分かる `as_of` から「メンテ中」にする
  (終わりが未定・記載なしなら `OPEN_FOR` まで)。日付だけの `as_of` からは「メンテ中」にしない。表を引く範囲 (`span`)・
  本文の取り直し (`needs_recheck`)・今日明日の並び・サイドバーの印・旧形式の日付も `as_of` を使う。
- 延長は `from → end`、日程変更は変更ごとの `from → to` を並べて出す。今日・明日は変更の後の時間帯で決める。
- バッジは、同じ記事で同じ時間帯・同じ状態の、対象だけが違うメンテナンスを 1 つにまとめる (3 つ以上は「PC版ほか 2 件」、
  ツールチップに全部の名前)。状態が違うものはまとめない。
- `AgendaDetail` の 2 つのバリアントは `Box` (clippy の `large_enum_variant` 対策)。組み立ては `AgendaDetail::from(..)`。
  `schedule_items.payload` は中身の構造体の JSON そのままで、足した項目は serde の既定値で前の形も読む。
- 日時は決められないときに推測しない (`Disposition::Unresolved`・`Note`)。曜日で年を動かさない、24:59 は翌日 0:59、
  終わりが始まりより前なら終わりを決めない、夏時間の重複・欠落は決めない、日付だけは `When::Date`。
- 抽出の評価は `cargo run -p gnn-schedule --release --example evaluate -- <nexus.db> --gold <正解.json>`。
  DB と正解はリポジトリに入れない。正解の `scope` は対象 1 つの名前 (複数の対象は対象ごとに 1 件)、`ongoing` は
  実施中の時点。
- hasami は最新のリリースのタグに固定する。上げるときは Cargo.toml の `tag`・Cargo.lock・
  `crates/gnn-schedule/dict/bundled.json` (リリースの `dictionaries.json` の ipadic の項目) をそろえ、
  `crates/gnn-app/assets/licenses/` の `LICENSE-IPADIC`・`THIRD_PARTY_LICENSES-HASAMI.md` も同じリリースのものにして、
  `mise run licenses` で一覧を作り直す (hasami の `LICENSE`・`LICENSE-APACHE-2.0`・文分割の例外表の NOTICE は一覧に
  入る。変わっていれば `packaging/licenses/about.toml` の hasami の SHA-256 を直す)。hasami の API は、Cargo.lock の
  commit の checkout (`~/.cargo/git/checkouts/hasami-*/<短い ID>`) で確かめる。
- gnn-cli を gnn-schedule に依存させない (カタログのリポジトリの CI が、辞書なしで `cargo install` する)。

### OS ごとの処理

- OS で分ける処理は `cfg(windows)` / `cfg(target_os = "macos")` で分け、ほかの OS の動きを変えない。
  OS で違う文言は定数にして各モジュールに置く (`autostart::SETTING_LABEL`、`tray::CLOSE_TO_TRAY_LABEL`・
  `tray::START_IN_TRAY_LABEL`、常駐する場所の呼び名の `tray::resident_place!()`、`shortcut::NOTIFY_SETTING_LABEL`、
  `ui::actions::REFRESH_TOOLTIP`)。
- Ctrl (macOS は ⌘) のキーは `ui/mod.rs` の `secondary!("r")` で書く。gpui-pre 0.3.11 の `KeyBinding::new` に
  `"secondary-r"` を渡すと起動時に panic する。
- ウィンドウを閉じる操作 (閉じるボタン・Ctrl+W・メニューの「閉じる」) は、`app::close_main_window` と
  `on_window_should_close` で同じ規則にする (「ウィンドウを閉じても残る」を切っていれば終了する)。
- `--hidden` の 2 つ目の起動は、起動中のアプリにウィンドウを出させずに終わる (`instance::ask_primary_to_show` を
  呼ばない)。
- `--quit` は起動中のアプリ (ロックを持つ最初の起動) に `quit` を送り、受けた側はトレイの「終了」と同じく
  `ServiceEvent::Quit` → `Cleanup` (`Services::shutdown`) で終わる。送った側は、Windows はプロセスが終わるまで
  (ロックを離してからプロセスが終わるため)、macOS はロックが外れるまで待つ (合わせて最大 30 秒、
  `instance::QUIT_TIMEOUT`)。受けた側の後始末は長くても `SHUTDOWN_TIMEOUT` (3 秒) とログの書き切り (1 秒ほど) なので、
  `QUIT_TIMEOUT` とインストーラーの待ち (10 秒) に収める。ロックファイル・データの
  ディレクトリ・ログを作らない (アンインストーラーは、一度も起動していないアプリにも呼ぶ)。終わらせるのは同じ
  実行ファイルで起動したアプリだけ (インストーラーが、同じデータの場所を使う開発中のビルドを終わらせないため)。
  終了コードは 0 (終わった・起動していなかった)・1 (伝えられない・終わらない・別の実行ファイル)・2 (引数の誤り)。
  `--hidden`・`--fetch-once`・`--quit` は一緒に使えない。`--fetch-once` はロックを取らないので、`--quit` は届かない。
  macOS の Dock・Finder・`open` からは 2 つ目のプロセスが起動しないので届かない (端末から .app の中の実行ファイルを
  起動する)。
- 2 つ目の起動との約束 (`instance.rs`): 最初の起動はポートのファイルに連絡先 (版・ポート・PID・実行ファイル・起動ごとの
  合言葉) を書き、依頼は「<種類> <合言葉>」、応答は「ok <PID>」。合言葉の合わない依頼には答えず、1 行の読み書きには
  期限を付ける。書き方を変えたら `PORT_FILE_VERSION` を上げる。`quit` を受けた後は `show` に答えず、答えをもらえ
  なかった 2 つ目の起動は、ロックを取り直して最初の起動になる (main.rs)。最初の起動はロックを離さずに終わり (OS が
  外す)、`--quit` はロックが外れたらプロセスが終わったとみなす。
- macOS は終了が `application().run(..)` から戻らずに exit() する。終わるときの後始末は app.rs の `Cleanup` に
  足す (macOS は `on_app_quit` から呼ぶ)。`on_app_quit` が返す future は gpui が 200ms しか待たないので、待つのは
  同期の部分で行う (`applicationWillTerminate` の中なので、`SHUTDOWN_TIMEOUT` より長く止めない)。待つ相手に
  メインスレッドを要る処理 (画面の更新) を入れない。
- macOS の Dock は、メインウィンドウを出すときに Regular、閉じたら Accessory にする (`macos.rs`)。メニューバーの
  アイコンは `tray-template.png` (黒と透明、36px = 18pt) をテンプレート画像として使う。
- キーやメニューの操作のハンドラ (`cx.on_action`) の中では、そのウィンドウを `update` できない (gpui が取り出して
  いる)。ウィンドウを触るときは `cx.defer` で後から行う (`macos.rs` の `with_active_window`)。
- macOS 向けのビルドは Mac で行う。Windows からの `cargo check --target aarch64-apple-darwin` は、C のコードを含む
  依存 (aws-lc-sys・libsqlite3-sys) と gpui-pre-apple のシェーダーの生成 (build.rs が macOS でだけ動く) で止まる。

### 配布物とリリース

- アプリの ID は変えない。macOS の bundle id・インストーラーの AppId・Windows の AppUserModelID (`shortcut.rs` の
  `APP_ID`) は、どれも `io.github.owayo.game-news-nexus`。ショートカットの名前・AppUserModelID・自動起動の登録名
  (`shortcut.rs`・`autostart.rs`) は `packaging/windows/game-news-nexus.iss` と、LaunchAgent の `LABEL` とアプリ名は
  `packaging/macos/Info.plist` と同じ値にする (`mise run installer`・`mise run bundle` が照らして、違えば止まる)。
  アプリは、スタートメニューのショートカットの行き先か AppUserModelID が違えば作り直す (違う ID では通知が出ない)。
- インストーラーは、更新でファイルを入れ替える前 (`PrepareToInstall`。Restart Manager が使用中のファイルを調べる前) と
  アンインストールの前に、入っている実行ファイルの `--quit` で通常の終了を頼み、プロセスが消えるのを待つ (動いている
  プロセスと同じ実行ファイルなので、頼み方は必ず通じる)。終わらなければ今までの方法 (更新は Restart Manager、
  アンインストールは taskkill) に戻す。画面を出しているときは先に確かめ、黙っているときは確かめない。
  インストーラーの検査 (`scripts/test-installer-windows.ps1`) が入れるアプリの代わりは `scripts/test-installer-fixture.cs`
  (csc.exe で作る) で、`--quit` の約束 (終了コード・終わるまで待つ) をまねる。約束を変えたら代わりも合わせる。
- Windows の実行ファイルの版の情報 (VERSIONINFO) は `assets/app.rc` に書き、版の数は build.rs が Cargo.toml の版から
  `version.rh` に書き出す。
- macOS の Info.plist に `LSUIElement`・`LSBackgroundOnly`・`NSSupportsAutomaticTermination` を入れない (Dock の表示は
  macos.rs が切り替える)。Info.plist を実行ファイルに埋め込まない (リンカの `-sectcreate __TEXT __info_plist` や
  embed_plist。.app の外で起動したときに通知の初期化で落ちる)。どちらも `mise run bundle` が断る。
- 版は Release のワークフローが決めて `Cargo.toml` と `Cargo.lock` に書く (日本時間の `yy.m.連番`、連番は月ごとに
  100 から)。手で上げない。
- Windows の配布物は C ランタイムを静的にリンクする (`.cargo/config.toml` の `+crt-static`)。既定の MSVC のビルドは
  VCRUNTIME140.dll を使い、入っていない PC で起動できない (`mise run installer` が断る)。
- 配布物には `LICENSE` と `crates/gnn-app/assets/licenses/` をそのまま入れる。実行ファイルに組み込んだ Rust のクレートの
  ライセンスの一覧 (`THIRD_PARTY_LICENSES.md`) は `mise run licenses` が cargo-about で作ってコミットする (手で直さない)。
  依存か `packaging/licenses/` を変えたら作り直す (CI の `mise run licenses -- --check` が古さを確かめる)。ここに手で
  置くのは、クレートのライセンスで表せない第三者のもの (埋め込む IPAdic の辞書など) だけ。
- 一覧は gnn-app の依存のうち、配る 2 つのターゲット (`about.toml` の `targets`) で実行ファイルに入るもの
  (build-dependencies・dev-dependencies を除く) を 1 つのファイルにまとめる。配るターゲットを足したら `targets` も足す。
  自分のクレートは `publish = false` で一覧から外す (ワークスペースにクレートを足したら付ける)。
- 使ってよいライセンスは `packaging/licenses/about.toml` の `accepted` (先に書いたものを選ぶ。MIT が先頭)。足すときは
  配布物で守る条件を確かめる。特定のクレートにだけ許すものは、そのクレートの `accepted` に書く (hasami の NAIST-2003 など)。
- cargo-about は本文のファイルを拾えないと、黙って SPDX の標準の文面に置き換える。原文があれば `about.toml` の `clarify`
  (パッケージの中は `files`、公開時のコミットのリポジトリは `git`。SHA-256 は `cargo about clarify` で計算する) で
  指定し、どこにもないことを確かめたときだけ `allowlist.toml` の `standard-text` に理由を書く。Cargo.toml の authors を
  著作権表示の代わりにしない。直下の NOTICE と下の階層のライセンスのファイルは、実行ファイルに入るなら `clarify` の
  `files` に、入らないなら `allowlist.toml` の `unlisted-file` に理由を書く。cargo-about の workaround は使わない (当たら
  なくても debug のログしか出ない)。`max-depth = 1` を外さない (頭にライセンスの文があるソースファイルが本文に丸ごと入る)。
- cargo-about の版は mise.toml の `licenses` のタスクの `tools` で固定する (depup は上げない。gnn-licenses が使った版と
  照らす)。上げたら `mise run licenses` で作り直し、差分を確かめる。
- PowerShell のスクリプトは Windows PowerShell 5.1 で動く書き方にし、UTF-8 の BOM 付きで保存する (CI が確かめる)。
  5.1 でモジュールの関数になっているコマンド (`Get-FileHash`・`New-TemporaryFile`・`New-Guid`・`Format-Hex`・
  `Import-PowerShellDataFile` など) は使わず、.NET で書く (PowerShell 7 から mise などを挟んで 5.1 を起動すると、7 の
  `PSModulePath` を受け継いで読み込めない。CI の既定のシェルは pwsh)。シェルスクリプトは macOS の bash 3.2 で動く
  書き方にする。
