# 開発と保守

構成、UI・取得・保存の制約、配布手順は [CLAUDE.md](CLAUDE.md) を参照する。

- ツールは `mise install`、辞書は `mise run dict` で用意する。
- 検証は `mise run lint`、`mise run test`、`mise run catalog-check`、`mise run licenses -- --check`。
- 動作確認には一時ディレクトリを `GNN_HOME` に指定し、利用者のデータを使わない。
- 画面はお知らせ・今日の予定・ソース・設定。`GNN_DEV_PAGE` は `news`・`agenda`・`sources`・`settings` を使う。
  色は `crates/gnn-app/assets/theme.json` と `ui::theme::Palette` から取り、共通の見た目は `ui/components/` に置く。
- カタログの取得先は `CATALOG_URL` に固定し、設定項目やフォルダ選択は設けない。
- 購読の読み取り・初期化・保存は `ConfigStore` を通す。`subscriptions.lock` は削除・置換しない。
  OS のファイルロックでプロセス間も排他し、取得できないときは保存せず誤りを返す。
- 起動時は `initialize_subscriptions`、変更時は `update_subscriptions` を使う。古いメモリ上の購読をそのまま保存しない。
  カタログ追加の取り込みも、ロックを取って最新の購読を読んでから行う。
- 購読の競合テストは `cargo test -p gnn-core config::tests`。書き込み途中のフックと子プロセスで、
  ファイルが一時的に見つからない状態・初回起動の競合を再現する。時間で競合を起こそうとしない。
- `subscriptions.lock` は処理ごとに開き、処理が終わると明示的にロックを外して閉じる。
  子プロセスなどに複製されたハンドルが残っていても、そこで排他区間を終える。常駐や通信のあいだは保持しない。
  同じ処理内で再取得せず、ロック取得済みの内部関数を使う。
- コード分析は `astro-sight`、コミットは日本語の Conventional Commits と詳細な本文を使う。
- 購読更新の手動ベンチマークと測定条件は [docs/benchmarks/subscriptions.md](docs/benchmarks/subscriptions.md)。
