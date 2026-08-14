# Cloudflare Tunnel で共有する

Cloudflare Tunnel を使うと、開発マシンの外部から環境にアクセスできるように
なります。スマートフォン、レビュアー、到達が必要な webhook などが対象です。

::: danger 環境をインターネットに公開する操作です
Minato は Cloudflare Access のポリシーを適用できません。適用には Cloudflare の
API が必要ですが、Minato の操作はすべて `cloudflared` CLI を経由するためです。
ポリシーの存在を保証できないため、`--public` を指定しない限り公開せず、
実行のたびに警告を表示します。

ホスト名に対する Access ポリシーは、利用者側で設定してください。
:::

## 必要なもの

- ドメインを登録した Cloudflare アカウント
- `cloudflared`（`brew install cloudflared`）

## 設定手順

```console
$ cloudflared tunnel login
```

このコマンドはブラウザを開いて入力を待ちます。Minato が代行しないのは、
daemon 内で対話的なプロンプトが表示されるとエージェントが応答できず停止する
ためです。`minato setup` が、応答できる端末がない場合には何も実行しないのと
同じ理由になります。

ログイン後の処理は Minato が実行します。

```console
$ minato tunnel enable --domain example.com --public
  ✓ starting the tunnel
╭ tunnel ─────────────────────────────────────────────────────────────────╮
│ running  *.example.com                                                  │
│                                                                         │
│ DNS  *.example.com                                                      │
│                                                                         │
│ this environment is reachable from the internet.                        │
│ Minato cannot see whether a Cloudflare Access policy is in front of it. │
╰─────────────────────────────────────────────────────────────────────────╯
```

named tunnel の作成、ゾーン全体のワイルドカード DNS レコードの登録、
`cloudflared` の起動を行います。いずれも冪等なため、繰り返し実行しても問題
ありません。

レコードはゾーン全体を覆いますが、明示的なレコードはワイルドカードより優先
されるため、そのドメインで既に公開しているものはそのまま応答します。Minato が
知らないホスト名はローカルのプロキシに到達し、404 が返ります。

## URL

```console
$ minato status
╭ myapp / feature-auth ──────────────────────────────────╮
│ feature/auth  /path/to/myapp.wt/feature-auth           │
│                                                        │
│ ● web  ready  https://web.feature-auth.myapp.localhost │
│                                                        │
│ shared over the tunnel:                                │
│ web  https://web-feature-auth-myapp.example.com        │
╰────────────────────────────────────────────────────────╯
```

トンネル側のホスト名は、サービス名・workspace 名・プロジェクト名を `-` で連結
した 1 ラベルになります。ローカルの URL がパートごとにサブドメインを分けるのとは
異なる形ですが、これは証明書の都合です。Cloudflare の Universal SSL は 1 階層目の
サブドメインまでしか覆わないため、それより深いホスト名は TLS のハンドシェイクで
拒否されます。トンネルは起動していて平文 HTTP なら応答するので、証明書の問題には
まず見えません。1 ラベルであれば無料の証明書の範囲に収まります。

`expose = false` のサービスにはトンネル側のホスト名が割り当てられません。
データベースは、ホスト名を推測されても外部から到達できません。

## 停止中の環境も起動する

レビュアーからの最初のリクエストでも、ローカルと同様に停止中の環境が起動
します。1〜2 秒待つとページが表示されます。

これはルーティングの構造から自然に得られる挙動です。トンネル側のホスト名は
プロキシのルーティングテーブルに `.localhost` と並べて登録され、同じサービスを
参照します。どちらも通常のルートとして扱われます。

## 停止する

```console
$ minato tunnel disable
╭ tunnel ─────────────────╮
│ disabled  *.example.com │
╰─────────────────────────╯
```

`cloudflared` を停止し、トンネル側のホスト名を削除します。named tunnel と DNS
レコードは Cloudflare 側に残ります。維持コストがかからず、残しておけば再開時に
ログインが不要になるためです。

ドメインは保持されるため、次回以降は次のように実行できます。

```console
$ minato tunnel enable --public
```

## 状態を確認する

```console
$ minato tunnel status
$ minato doctor | grep -i tunnel
```

`status` は何も実行しません。設定が完了していない場合は、残りの手順を表示
します。

| 状態 | 意味 |
| --- | --- |
| `disabled` | 未設定、または停止中 |
| `not installed` | `cloudflared` が存在しない |
| `needs login` | `cloudflared tunnel login` が未実行 |
| `stopped` | 設定済みだが稼働していない |
| `running` | 通信を中継している |

daemon は再起動時に、有効化されていたトンネルを復元します。共有した URL は
そのまま使用できます。

## 構成

1 台のマシンにつき 1 本の named tunnel が、すべてのプロジェクトを中継します。
ingress ルールは 1 つのみで、ゾーン全体をローカルのプロキシへ転送し、Host に
よる振り分けはプロキシが担当します。DNS レコードもワイルドカード 1 件
（`*.example.com`）のみのため、プロジェクトや worktree が増減しても DNS の更新も
`cloudflared` の再読み込みも発生しません。

`cloudflared` からプロキシまでの区間は、ループバック上の平文 HTTP です。TLS は
Cloudflare のエッジで終端されており、また `cloudflared` にローカル CA を信頼
させる理由もないためです。

::: tip 実際のゾーンで確認済みです
ホスト名のルーティング、生成される設定ファイル、CLI に渡す引数、トンネル経由
での自動起動については、スタブを使ったテストで検証しています。加えて `enable`
は実際の Cloudflare ゾーン（Free プラン）に対しても実行済みで、ワイルドカード
レコードが作成され、トンネルの URL がゾーンの Universal SSL 証明書のまま https
で応答することを確認しています。追加の購入も手動設定も必要ありません。
:::
