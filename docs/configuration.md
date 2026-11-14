# 設定

## データの場所

| 内容 | 場所 |
| --- | --- |
| アプリ設定 | `%APPDATA%\game-news-nexus\config\settings.toml` |
| ゲームの on/off と手元だけの上書き (アイコン・取得間隔など) | `%APPDATA%\game-news-nexus\config\subscriptions.toml` |
| 手元の取得の定義 | `%APPDATA%\game-news-nexus\config\sources\<id>.yaml` |
| 秘密の値 | `%APPDATA%\game-news-nexus\config\secrets.toml` |
| 記事 (SQLite) | `%APPDATA%\game-news-nexus\data\nexus.db` |
| 取り込んだカタログの控え | `%APPDATA%\game-news-nexus\data\catalog.json` |
| 切り抜いたアイコン | `%APPDATA%\game-news-nexus\data\icons\` |
| ログ | `%APPDATA%\game-news-nexus\data\logs\` |
| 起動中のアプリのロックと連絡先 (二重起動の防止と `--quit`。`instance.port` には起動ごとの合言葉が入る) | `%APPDATA%\game-news-nexus\data\instance.lock`・`instance.port` |
| 画像キャッシュ | `%LOCALAPPDATA%\game-news-nexus\cache\images\` (最大 1 GB、最後に確かめてから 180 日。配信元の ETag・Last-Modified と、書いたときの長さ・SHA-256 は画像の横の `.meta`) |

macOS では、Windows の `config` と `data` に置くものを `~/Library/Application Support/game-news-nexus/` にまとめて置き (例: `settings.toml`・`sources/<id>.yaml`・`nexus.db`・`logs/`)、画像キャッシュは `~/Library/Caches/game-news-nexus/images/` に置きます。

環境変数 `GNN_HOME` を指定すると、すべてをそのディレクトリの下 (`config`・`data`・`cache`) に置きます。手で編集した場合は、アプリを起動し直すと読み込みます。

## カタログ

ゲームごとの取得の定義は [game-news-nexus-config](https://github.com/owayo/game-news-nexus-config) に 1 ゲーム 1 ファイルの YAML で置いています。有志の PR で増やしていく前提です。

アプリはカタログの写しを同梱していて、初めて起動したときはそれを使います。起動時と 1 日 1 回、カタログの `main` を取り込みます (変わっていなければ取り直しません)。

初めて起動したときは、DMM GAMES と HoYoverse (原神・崩壊：スターレイル・ゼンレスゾーンゼロ) のグループのゲームが on になります。ゲームメディア (4Gamer.net・GAME Watch・AUTOMATON) と Steam のニュースは、ソース画面で on にできます。

あとからカタログに加わったゲームも、既定で on のグループのものは初めて取り込んだときに on になります。off にしたゲームは、カタログが更新されても off のままです。

取り込みはファイルごとに判断します。読めないファイルや、このアプリより新しい版を求めるファイル (`min_app_version`) は、そのゲームの前の版を使い続けます。前の版がなければ、ソース画面に「アプリ X 以降」と出します。

取得先は上記のリポジトリに固定しています。設定画面では取り込み状況を確認でき、「今すぐ取り込む」で更新できます。

### 一般版と FANZA 版 (成人向けのソース)

DMM GAMES のゲームには、一般向けの版 (DMM GAMES) と成人向けの版 (FANZA GAMES) があるものがあります。カタログでは版ごとに別のソースにしていて、成人向けの版の定義には `adult: true` と、同じゲームの一般向けの版の ID (`general_edition`) を書きます。FANZA GAMES だけのゲームは `adult: true` だけを書きます。

- 既定では成人向けのソースを使いません。取得も、ソース画面への表示もしません (まだ取り込めない成人向けのソースの名前も出しません)。
- 設定の「成人向けのソースを使う (FANZA GAMES など)」を入れると、18 歳以上であることの確認が出ます。「私は 18 歳以上です」に印を付けて「同意して有効にする」を押すと有効になり、同意した時刻を `settings.toml` の `adult_consented_at` に残します。入れるたびに確かめます。
- 有効にすると、一般向けの版のあるゲームは、その版の代わりに成人向けの版を取得します。on/off・アイコン・取得間隔は一般向けの版の設定をそのまま使います。FANZA GAMES だけのゲームはソース画面に出るので、読みたいものを on にします (既定では off)。
- 切ると、成人向けのソースの取得をやめ (取得中のものも取り消します)、そのお知らせを一覧・今日の予定・通知・記事を直接開く操作・旧形式の書き出しから隠します。保存した記事は消さず、もう一度有効にすると戻ります。
- 手元の定義でも `adult: true` と書けば同じ扱いです。カタログの成人向けのソースは、手元で上書きしても一般向けにはできません。版の組 (`general_edition`) はカタログの定義で決まり、手元だけのソースでは使えません。
- 一般向けの版のソースで、これまで成人向けの版の取得先から取得していた記事は、成人向けの版の定義に `inherit_articles: true` があれば、初めてその組を読み込んだときに 1 度だけ成人向けの版へ移します (既読・予定もそのまま)。

## 取得の定義

取得の定義は YAML で、1 ファイルに 1 つのゲームを書きます。処理の流れは段で決まっていて、各段で何をするかを書きます。

1. `prepare` (任意): 事前に取得し、応答から値を取り出して `{{vars.名前}}` にする
2. `lists`: 一覧を取得して記事の候補を作る。複数書くと宣言順にまとめる。上限付きのページ送りもできる
3. まとめ (アプリが行う): 重複・取得済み・古い記事を除く
4. `detail` (任意): 残った記事だけ詳細ページを取る
5. `body`: 本文を整える (置換・要素の除去・改行など)

```yaml
# ミストトレインガールズ
schema: 2
min_app_version: '26.11.6'
id: mist_train_girls
name: ミストトレインガールズ
lists:
  - kind: json
    request:
      url: 'https://fd-misttrain-prod.azurefd.net/api/info/news?gameVersion=2'
    items: '/articles'
    filter:
      - { pointer: '/visible', eq: true }
    fields:
      id: '/articleUrl'
      title: '/title'
      published: '/date'
      image: '/bannerImgUrl'
detail:
  kind: html
  request:
    url: 'https://fd-misttrain-prod.azurefd.net/api/info/news?id={{item.id}}'
body:
  image_in_body: true
```

書き方の詳細 (取得方式・取り出す項目・条件・ページ送り・差し込み・本文の整形) は、カタログの [CONTRIBUTING.md](https://github.com/owayo/game-news-nexus-config/blob/main/CONTRIBUTING.md) にまとめています。手元の定義もカタログと同じ書き方です。

- YAML は 1.2 の決まりで厳格に読みます。文字列は単一引用符で囲んでください (`#` で始まるセレクタや `{{` で始まるテンプレートを YAML の記号と取り違えないため)。アンカー・エイリアス・マージ・独自タグは使えません。
- アプリのソース画面では、フォームか YAML タブで書けます。YAML タブで書いたコメントは、フォームで値を変えない限りそのまま保存します。
- 記事は `id` → `url` → `title` の順で見分けます。訂正で新着扱いにならないよう、できるだけ `id` を指定してください。
- 本文の予定 (メンテナンスの日時など) を読む地域時刻は、投稿日時の `timezone` とは別に `schedule:` で書けます。HoYoverse の 3 タイトルは、本文の日時を日本時間で、「サーバー時間」と書かれた日時を UTC+8 で読みます。`schedule:` を変えても取得し直さず、保存済みの記事から読み直します。
- 画像の URL に署名・期限や取得した時刻 (キャッシュ破り) の問い合わせが付き、同じ画像でも取得のたびに URL が変わる配信元は、`url_identity:` に外す問い合わせを書きます。書いた画像は、記事が変わったかを比べるときと画像のキャッシュでは同じ画像として扱い (署名が変わっても「更新」にせず、取り直さない)、取りに行くときは配信元の URL をそのまま使います (署名を外すと取れない配信元があるため)。問い合わせを推測で外すことはしません (`?v=2` のような版は、中身が変わった印として扱います)。ガールズクリエイション・ドットアビス・Twinkle☆Star Knights で使っています。キャッシュになく、記録した URL の署名の期限も切れていた画像は、そのソースを取り直して新しい URL で取ります (詳細ページの画像は、その記事の詳細も取り直します。「制限」も見てください)。

  ```yaml url_identity: images: - origin: 'https://cdn.gc.dmmgames.com' path_prefix: '/secure/img/' ignore_query: ['s', 't'] ```

### 秘密の値 (`secrets.toml`)

手元の定義では、ヘッダの値に `{{secret:名前}}` と書くと `secrets.toml` の値に置き換えます。カタログの定義では使えません (第三者が書いた定義に、利用者の秘密の値を送らせないため)。

```toml
[values]
mist_cookie = "session=..."
```

### 取得先の制限

- カタログの定義は、インターネット上のアドレスだけに取得します。localhost・LAN・リンクローカルへは、名前解決したアドレスとリダイレクト先も確かめて接続しません。
- 手元の定義も、既定では同じ制限です。自分のサーバーなどを読むときは、編集画面の 「手元のネットワーク (localhost・LAN) への取得を許す」を on にします。
- 本文の画像は、どの定義のものでも手元のネットワークへは取りに行きません。
