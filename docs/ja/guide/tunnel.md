# トンネルで共有する

Cloudflare Tunnel を使うと、環境をマシンの外から —— スマホ、レビュアー、
届く必要のある webhook から —— 到達可能にできます。

::: danger これは環境をインターネットに置く操作です
Minato は Cloudflare Access のポリシーを適用できません。それには Cloudflare の
API が必要で、ここでの操作はすべて `cloudflared` CLI 経由だからです。
ポリシーがあると保証できない以上、`--public` なしでは何も公開せず、毎回その
旨を表示します。

ホスト名の前に Access ポリシーを自分で置いてください。
:::

## 必要なもの

- ドメインが載った Cloudflare アカウント
- `cloudflared`（`brew install cloudflared`）

## 設定する

```console
$ cloudflared tunnel login
```

ブラウザが開いて待ちます。Minato が代わりに実行しないのは、daemon の中で
対話プロンプトが出るとエージェントが答えられない場所で固まるからで、
`minato setup` が `sudo` コマンドを実行せず提示するのと同じ理由です。

ログイン後は Minato がやります。

```console
$ minato tunnel enable --domain example.com --public
  ✓ starting the tunnel
tunnel: running  (*.example.com)
  DNS:   *.myapp.example.com

  This environment is reachable from the internet.
  Minato cannot see whether a Cloudflare Access policy is in front of it.
```

named tunnel を作り、プロジェクト用のワイルドカード DNS レコードを張り、
`cloudflared` を起動します。すべて冪等なので、もう一度実行しても構いません。

## URL

```console
$ minato status
  web   ready     https://web.feature-auth.myapp.localhost

  shared over the tunnel:
  web   https://web-feature-auth.myapp.example.com
```

トンネルのホスト名はサービスと workspace を `-` で繋ぎます。トンネル側の
ホスト名はサブドメインが 1 段しか確実に使えないためです。

`expose = false` のサービスにはトンネルのホスト名がありません。データベースは
名前を推測しても外から届きません。

## scale-to-zero もそのまま効く

レビュアーの最初のリクエストが、ローカルと同じように停止中の環境を起こします。
1〜2 秒待ってページが出ます。

これはルーティングの仕組みから自然に出てきます。トンネルのホスト名は
プロキシのルーティングテーブルに `.localhost` の隣として登録され、同じ
サービスを指します。どちらも普通のルートです。

## 止める

```console
$ minato tunnel disable
tunnel: disabled  (*.example.com)
```

`cloudflared` を止め、トンネルのホスト名を落とします。named tunnel と DNS
レコードは Cloudflare に残ります。放っておいてもコストはかからず、残して
おけば再開にログインが要らないからです。

ドメインは覚えているので、次からは:

```console
$ minato tunnel enable --public
```

## 状況を見る

```console
$ minato tunnel status
$ minato doctor | grep -i tunnel
```

`status` は何も実行しません。設定が途中なら、残っているコマンドを出します。

| 状態 | 意味 |
| --- | --- |
| `disabled` | 未設定、または止めた |
| `not installed` | `cloudflared` が無い |
| `needs login` | `cloudflared tunnel login` がまだ |
| `stopped` | 設定済みだが動いていない |
| `running` | トラフィックを流している |

daemon は再起動時に、有効だったトンネルを復帰させます。誰かに渡したリンクは
そのまま生きます。

## どう構成されているか

マシンに 1 本の named tunnel がすべてのプロジェクトを運びます。ingress ルールは
1 つで、ゾーン全体をローカルのプロキシに送り、Host での振り分けはプロキシに
任せます。プロジェクトごとに 1 つのワイルドカードレコード
（`*.myapp.example.com`）なので、worktree が増減しても DNS も `cloudflared`
の再読み込みも発生しません。

`cloudflared` からプロキシへの区間はループバック上の平文 HTTP です。TLS は
Cloudflare のエッジで終端され、`cloudflared` にローカル CA を信頼する理由は
ありません。

::: tip スタブでの検証です
ホスト名のルーティング、生成される設定、CLI の引数、トンネル経由の
scale-to-zero はテストされています。実行されていないのは、実際のゾーンに対する
実際の named tunnel です。想定外の挙動があれば、Cloudflare のプランが
ワイルドカードの DNS レコードを許しているか確認してください。
:::
