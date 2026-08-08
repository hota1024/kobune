# プレビューを共有する

ブランチの環境をインターネットに公開し、スマートフォン、デザイナー、webhook
などからアクセスできるようにします。

::: danger 事前にお読みください
トンネルを有効化すると、URL を知っている人であれば誰でも開発環境にアクセス
できます。Minato は Cloudflare Access のポリシーを**適用できません**。適用には
Cloudflare の API が必要ですが、Minato の操作はすべて `cloudflared` CLI を
経由するためです。したがって、アクセス制御がかかっていることを保証できません。

ホスト名に対する Access ポリシーは、利用者側で設定してください。Minato は
`--public` を指定しない限り公開せず、実行のたびに警告を表示します。
:::

ドメインを登録した Cloudflare アカウントが必要です。

## インストールとログイン

```console
$ brew install cloudflared
$ cloudflared tunnel login
```

このコマンドはブラウザを開きます。Minato が代行しないのは、daemon 内で対話的な
プロンプトが表示されるとエージェントが応答できず停止するためです。
`minato setup` が、応答できる端末がない場合には何も実行しないのと同じ理由です。

## 有効化する

```console
$ minato tunnel enable --domain example.com --public
  ✓ starting the tunnel
╭ tunnel ─────────────────────────────────────────────────────────────────╮
│ running  *.example.com                                                  │
│                                                                         │
│ DNS  *.myapp.example.com                                                │
│                                                                         │
│ this environment is reachable from the internet.                        │
│ Minato cannot see whether a Cloudflare Access policy is in front of it. │
╰─────────────────────────────────────────────────────────────────────────╯
```

内部では named tunnel の作成、プロジェクト用ワイルドカード DNS レコードの
登録、`cloudflared` の起動を行っています。いずれも冪等なため、繰り返し実行
しても問題ありません。

## 共有する URL

```console
$ minato status -w feature-checkout
╭ myapp / feature-checkout ──────────────────────────────────╮
│ feature/checkout  /path/to/myapp.wt/feature-checkout       │
│                                                            │
│ ● web  ready  https://web.feature-checkout.myapp.localhost │
│                                                            │
│ shared over the tunnel:                                    │
│ web  https://web-feature-checkout.myapp.example.com        │
╰────────────────────────────────────────────────────────────╯
```

共有するのは後者の URL です。サービス名と workspace 名が `-` で連結されて
いるのは、トンネル側のホスト名ではサブドメインを 1 階層しか確実に扱えない
ためです。

```console
$ minato status -w feature-checkout --json \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["workspace"]["services"][0]["tunnel_url"])'
```

## 共有先から見た挙動

- **停止中の環境も利用できます。** 最初のリクエストで環境が起動し、1〜2 秒で
  応答します。ローカルからのアクセスと同じ動作で、トンネル側のホスト名も
  同じルーティングテーブル上の通常のルートとして扱われるためです。
- **公開したサービスのみアクセスできます。** `expose = false` を指定した
  サービス、たとえばデータベースにはトンネル側のホスト名が存在せず、
  推測されても到達できません。
- **正規の証明書が使われます。** TLS は Cloudflare のエッジで終端されるため
  警告は表示されず、証明書を信頼させる作業も不要です。ローカル CA は関与
  しません。

## Access を設定する

この作業は Minato では実行できません。Cloudflare のダッシュボードで
Zero Trust → Access → Applications を開き、`*.myapp.example.com` に対する
self-hosted application とポリシーを作成してください。メールドメインによる
制限や、社外の相手にはワンタイム PIN が利用できます。

公開 Web サーバに置けないものを共有する前に、必ず設定してください。

## 停止する

```console
$ minato tunnel disable
╭ tunnel ─────────────────╮
│ disabled  *.example.com │
╰─────────────────────────╯
```

トンネル側のホスト名は即座に無効になり、ローカルの URL には影響しません。
named tunnel と DNS レコードは Cloudflare 側に残るため、再開時にログインは
不要です。

```console
$ minato tunnel enable --public
```

## 再起動後の挙動

daemon は再起動時に、有効化されていたトンネルを復元し、ルーティングテーブルも
再構築します。共有した URL は、マシンを再起動しても引き続き使用できます。

## 問題が起きた場合

```console
$ minato tunnel status
$ minato doctor | grep -i tunnel
$ tail -f ~/.minato/logs/minatod.log   # cloudflared のログもここに出力されます
```

| 症状 | 想定される原因 |
| --- | --- |
| `needs login` | `cloudflared tunnel login` が未実行 |
| `not installed` | `brew install cloudflared` が必要 |
| 有効なのに `stopped` | `cloudflared` が終了した。daemon のログを確認 |
| Cloudflare 1016 | DNS レコードが存在しない。`tunnel enable` を再実行 |
| ワイルドカードレコードが拒否される | Cloudflare のプランが対応していない可能性 |

::: tip 検証はスタブによるものです
ルーティング、生成される設定ファイル、CLI に渡す引数、トンネル経由での自動
起動については、テストで検証しています。未検証なのは、実際のゾーンに対する
実際の named tunnel での動作です。
:::

## 次に読むもの

- [Cloudflare Tunnel で共有する](../guide/tunnel) — 構成とその設計理由
- [AI エージェントと使う](../guide/agents) — エージェントが `tunnel enable` を
  自ら実行すべきでない理由も含みます
