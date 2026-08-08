# プレビューを共有する

ブランチの環境をインターネットに置き、スマホ・デザイナー・webhook から
届くようにします。

::: danger 先に読んでください
トンネルを張ると、URL を知っている人なら誰でも開発環境に到達できます。
Minato は Cloudflare Access のポリシーを **適用できません** —— それには
Cloudflare の API が必要で、ここでの操作はすべて `cloudflared` CLI 経由です
—— ので、何かが守っていると保証できません。

ホスト名の前に Access ポリシーを自分で置いてください。Minato は `--public`
なしでは何も公開せず、毎回警告を繰り返します。
:::

ドメインが載った Cloudflare アカウントが必要です。

## インストールしてログインする

```console
$ brew install cloudflared
$ cloudflared tunnel login
```

ブラウザが開きます。Minato が代わりに実行しないのは、daemon の中の対話
プロンプトがエージェントを答えられない場所で固まらせるからで、
`minato setup` が `sudo` コマンドを実行せず提示するのと同じ理由です。

## 有効にする

```console
$ minato tunnel enable --domain example.com --public
  ✓ starting the tunnel
tunnel: running  (*.example.com)
  DNS:   *.myapp.example.com

  This environment is reachable from the internet.
  Minato cannot see whether a Cloudflare Access policy is in front of it.
```

裏では named tunnel を作り、プロジェクト用のワイルドカード DNS レコードを
張り、`cloudflared` を起動しています。すべて冪等なので、もう一度実行しても
構いません。

## リンク

```console
$ minato status -w feature-checkout
  web   ready     https://web.feature-checkout.myapp.localhost

  shared over the tunnel:
  web   https://web-feature-checkout.myapp.example.com
```

後者を送ります。サービスと workspace が `-` で繋がっているのは、トンネルの
ホスト名がサブドメインを 1 段しか確実に扱えないためです。

```console
$ minato status -w feature-checkout --json \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["workspace"]["services"][0]["tunnel_url"])'
```

## レビュアーから見えるもの

- **停止中でも動きます。** 最初のリクエストが 1〜2 秒で環境を起こします。
  ローカルとまったく同じで、トンネルのホスト名は同じテーブルの普通のルート
  だからです。
- **公開したサービスだけ届きます。** `expose = false` のもの —— データベース
  —— にはトンネルのホスト名が無く、推測しても届きません。
- **本物の証明書です。** TLS は Cloudflare のエッジで終端するので警告は出ず、
  信頼させるものもありません。ローカル CA は関係しません。

## Access を設定する

ここは Minato にはできません。Cloudflare のダッシュボードで
Zero Trust → Access → Applications から、`*.myapp.example.com` に対する
self-hosted application とポリシーを作ります。メールドメイン、または社外の
人には ワンタイム PIN を。

公開 Web サーバに置きたくないものを共有する前に、必ず設定してください。

## 止める

```console
$ minato tunnel disable
tunnel: disabled  (*.example.com)
```

トンネルのホスト名はすぐにルーティングされなくなり、ローカルの URL は
そのままです。named tunnel と DNS レコードは Cloudflare に残るので、
再開にログインは要りません。

```console
$ minato tunnel enable --public
```

## 再起動をまたいで

daemon は再起動時に、有効だったトンネルを復帰させ、ルーティングテーブルも
作り直します。誰かに渡したリンクは、あなたが再起動しても生きています。

## うまくいかないとき

```console
$ minato tunnel status
$ minato doctor | grep -i tunnel
$ tail -f ~/.minato/logs/minatod.log   # cloudflared のログもここに出ます
```

| 症状 | 原因 |
| --- | --- |
| `needs login` | `cloudflared tunnel login` がまだ |
| `not installed` | `brew install cloudflared` |
| 有効なのに `stopped` | `cloudflared` が終了した。daemon のログを確認 |
| Cloudflare 1016 | DNS レコードが無い。`tunnel enable` をやり直す |
| ワイルドカードのレコードが拒否される | Cloudflare のプランが許していない可能性 |

::: tip スタブでの検証です
ルーティング、生成される設定、CLI の引数、トンネル経由の scale-to-zero は
テストされています。実行していないのは、実際のゾーンに対する実際の
named tunnel です。
:::

## 次に

- [トンネルで共有する](../guide/tunnel) — どう構成され、なぜそうなのか
- [AI エージェントと使う](../guide/agents) — エージェントが `tunnel enable` を
  自分で実行すべきでない理由も
