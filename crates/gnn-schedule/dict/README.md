# 同梱する辞書

`gnn-schedule` は、hasami の IPAdic 辞書 (`ipadic.hsd`) を実行ファイルに埋め込みます。
辞書はリポジトリに入れず (`.gitignore`)、`mise run dict` が hasami のリリースから取ってここに置きます。
ビルドは通信せず、`build.rs` が大きさと SHA-256 を `bundled.json` と照合するだけです。

| ファイル | 中身 |
| --- | --- |
| `bundled.json` | 埋め込む辞書の固定情報。hasami のリリースのタグと辞書の形式の版、リリースに添付された `dictionaries.json` の `ipadic` の項目 (大きさ・SHA-256。圧縮版も) |
| `ipadic.hsd` | 辞書の本体。`mise run dict` が置く (コミットしない) |

hasami を上げるときは、`Cargo.toml` の hasami のタグと `bundled.json` を同じリリースにそろえます
(`dictionary` は、そのリリースの `dictionaries.json` の `ipadic` の項目をそのまま写す)。辞書のライセンスは
`crates/gnn-app/assets/licenses/` に置き、配布物に同梱します。
