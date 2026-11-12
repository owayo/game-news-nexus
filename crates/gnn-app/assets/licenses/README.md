# 第三者のライセンス

配布物 (Windows のインストーラーと macOS の .app) に同梱する、アプリに入っている第三者のもののライセンスです。
このフォルダごと、配布物の `licenses` に入ります。

| ファイル | 対象 |
| --- | --- |
| `THIRD_PARTY_LICENSES.md` | 実行ファイルに組み込んだ Rust のクレート (間接の依存を含む) のライセンスの一覧。hasami (文分割の例外表の NOTICE を含む)・gpui-kit (画面のアイコンの Lucide を含む) などもここにある。`mise run licenses` が作る (手で直さない) |
| `LICENSE-IPADIC` | 実行ファイルに埋め込む IPAdic 辞書 (hasami の `ipadic.hsd`。mecab-ipadic 2.7.0-20070801)。NAIST-2003 |
| `THIRD_PARTY_LICENSES-HASAMI.md` | 固定した版の hasami の第三者の一覧 (そのまま写したもの)。このアプリが含むのは IPAdic の辞書と文分割の例外表で、NEologd・SudachiDict の辞書は含まない |
| `LICENSE-APACHE-2.0` | Apache License 2.0 の全文 (`THIRD_PARTY_LICENSES-HASAMI.md` が参照する) |

クレートのものは `THIRD_PARTY_LICENSES.md` にまとめ、辞書のようにクレートのライセンスでは表せないものだけを
ここに手で置きます。`THIRD_PARTY_LICENSES.md` は、依存か `packaging/licenses/` の設定を変えたら
`mise run licenses` で作り直してコミットします (古いと CI が止まる)。

hasami を上げたときは、そのリリースの `THIRD_PARTY_LICENSES.md` に合わせて `THIRD_PARTY_LICENSES-HASAMI.md` を
更新し、`mise run licenses` で一覧を作り直します (hasami の `LICENSE`・`LICENSE-APACHE-2.0`・
`src/sentence/builtin_exceptions.NOTICE` が変わっていれば、`packaging/licenses/about.toml` の SHA-256 も直す)。
