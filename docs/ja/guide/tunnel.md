# トンネルで共有する

トンネルを使うと、開発マシンの外部から環境にアクセスできるようになります。
スマートフォン、レビュアー、到達が必要な webhook などが対象です。

::: danger 環境をインターネットに公開する操作です
Kobune が扱うトンネルは、どれも環境の手前に認証を置きません。そのため
`--public` を指定しない限り公開せず、実行のたびに警告を表示します。

それが具体的に何を意味するかは、選んだトンネルによって変わります。`--public`
を付けずに実行すると、どちらの状況なのかを表示してから止まります。以下の該当
する節を読んでください。
:::

## どちらを使うか

```console
$ kobune tunnel enable --provider quick --public
$ kobune tunnel enable --provider cloudflare --domain example.com --public
```

| | `quick` | `cloudflare` |
| --- | --- | --- |
| アカウント | 不要 | ドメインを登録した Cloudflare アカウント |
| 事前設定 | 不要 | `cloudflared tunnel login` を 1 回 |
| ホスト名 | Cloudflare のもの。サービスごとに払い出される | 自分のゾーンのもの |
| URL の寿命 | トンネルを止めるまで | 消えない |
| 後から作った worktree | 再度 enable するまで届かない | すぐ届く |
| daemon の再起動 | 復元しない | 復元する |
| アクセス制御 | 適用する手段がない | Cloudflare Access のポリシーを自分で設定 |

`quick` は、いま誰かに見せるためのものです。`cloudflare` は、明日も使えるリンク
のためのものです。

選択は保持されるため、次回以降はどちらのフラグも不要です。

```console
$ kobune tunnel enable --public
```

どちらも `cloudflared` が中継するため、インストールは共通です。

```console
$ brew install cloudflared
```

## 手軽な方法

事前の設定はありません。アカウントもドメインもログインも不要です。

```console
$ kobune tunnel enable --provider quick --public
  ✓ starting the tunnel
╭ tunnel ─────────────────────────────────────────────────────╮
│ running  quick                                              │
│                                                             │
│ ! these URLs are Cloudflare's and last only as long as      │
│ ! this tunnel: restarting gives out different ones.         │
│ ! 2 services published; anything made later                 │
│ ! needs `kobune tunnel enable` again to be reachable.       │
│                                                             │
│ ! this environment is reachable from the internet.          │
│ There is no access control: anyone with the URL reaches it. │
╰─────────────────────────────────────────────────────────────╯
```

```console
$ kobune status
╭ myapp / feature-auth ────────────────────────────────────╮
│ feature/auth  /path/to/myapp.wt/feature-auth             │
│                                                          │
│ ● web  ready  https://web.feature-auth.myapp.localhost   │
│ ● api  ready  https://api.feature-auth.myapp.localhost   │
│                                                          │
│ shared over the tunnel:                                  │
│ web  https://restless-mode-plans-guru.trycloudflare.com  │
│ api  https://chapter-vhs-hometown-mill.trycloudflare.com │
╰──────────────────────────────────────────────────────────╯
```

### できないこと

**quick のトンネルは 1 つのホスト名で 1 つのサービスにしか届きません。** その
ため Kobune は、enable を実行した workspace の公開サービスごとに `cloudflared`
を 1 つずつ起動します。サービスが 3 つなら、プロセスも 3 つ、互いに無関係な
ホスト名も 3 つです。

以下の制約はすべてここから来ています。

**実行した時点で存在していたものだけを公開します。** あとから作成した worktree
にはホスト名が割り当てられず、`kobune.toml` に追加したサービスも同様です。
公開するには `kobune tunnel enable --public` を再実行します。

**URL は残りません。** ホスト名は Cloudflare が接続ごとに払い出すもので、
トンネルを止めると消滅します。起動し直すと別の名前になるため、午前中に共有した
リンクは午後にはもう繋がりません。

**daemon は再起動時に復元しません。** `kobune daemon restart` のあと、状態は
`stopped` になります。復元しても誰も知らないホスト名を新しく取得するだけで、
共有済みのリンクが指しているのは消えた古い名前だからです。再開は明示的に
実行します。

**手前に何かを置くことはできません。** ホスト名は Cloudflare のものであって
自分のものではないため、Cloudflare Access のポリシーを紐づける対象が存在しま
せん。URL を知っている人は誰でも環境に到達します。

## 自分のドメインで運用する

`cloudflare` は、自分が所有するゾーンの上で named tunnel を運用します。ホスト名
は自分のもので、消えず、Access のポリシーを手前に置けます。

### 必要なもの

- ドメインを登録した Cloudflare アカウント
- `cloudflared`（`brew install cloudflared`）

### 設定手順

```console
$ cloudflared tunnel login
```

このコマンドはブラウザを開いて入力を待ちます。Kobune が代行しないのは、
daemon 内で対話的なプロンプトが表示されるとエージェントが応答できず停止する
ためです。`kobune setup` が、応答できる端末がない場合には何も実行しないのと
同じ理由になります。

ログイン後の処理は Kobune が実行します。

```console
$ kobune tunnel enable --provider cloudflare --domain example.com --public
  ✓ starting the tunnel
╭ tunnel ───────────────────────────────────────────────────────╮
│ running  cloudflare  *.example.com                            │
│                                                               │
│ DNS  *.example.com                                            │
│                                                               │
│ ! *.example.com now points here.                              │
│ ! Names with a record of their own are unaffected;            │
│ ! any other name in the zone reaches this machine.            │
│                                                               │
│ ! this environment is reachable from the internet.            │
│ Kobune cannot see whether an access policy is in front of it. │
╰───────────────────────────────────────────────────────────────╯
```

named tunnel の作成、ゾーン全体のワイルドカード DNS レコードの登録、
`cloudflared` の起動をまとめて実行します。いずれも冪等なため、繰り返し実行して
も問題ありません。

Access のポリシーは Kobune が適用できません。適用には Cloudflare の API が必要
ですが、ここでの操作はすべて `cloudflared` CLI を経由するためです。ポリシーの
存在を保証できないため、実行のたびにその旨を表示します。ホスト名に対する
ポリシーは、利用者側で設定してください。

### ゾーンの指定

`--domain` にはゾーン自体を指定します（`dev.example.com` ではなく
`example.com`）。ゾーンの 1 階層下のホスト名は Universal SSL 証明書に覆われ
ますが、その下は覆われないためです。

さらに、**`cloudflared tunnel login` が対象としたゾーン**である必要があります。
login が書き出す証明書は 1 つのゾーンに紐づいており、`cloudflared tunnel route
dns` はその外側のホスト名を*そのゾーンからの相対名*として扱います。
`example.com` に対する login のまま `other.com` を指定すると、作られるのは
`*.other.com.example.com` で、コマンドは成功し、`*.other.com` は存在しないまま
になります。Kobune はレコードを登録したあとに名前が解決するか確認し、解決しない
場合はその旨を表示します。この状態は、他のどこを見ても異常に見えないためです。
トンネルは起動し、`status` は `running` を返し、URL にはいつまでも何も届き
ません。ゾーンを切り替えるには login をやり直してください。

レコードはゾーン全体を覆いますが、明示的なレコードはワイルドカードより優先
されるため、そのドメインで既に公開しているものはそのまま応答します。Kobune が
知らないホスト名はローカルのプロキシに到達し、404 が返ります。上の出力にある
注記がこれで、そのドメインで最初に `enable` を実行したときにだけ表示されます。

`*` レコードが既に存在していた場合は、代わりにその旨が表示されます。Cloudflare
は「その名前は使用済み」としか返さず、何を指しているかは分からないため、Kobune
にはそのレコードがこのトンネルに向いているか判断できません。向いていなければ、
ここまでの表示が `running` のままで、すべてのホスト名が別の場所へ流れます。
URL を信用する前に、ダッシュボードでそのレコードを確認してください。

## URL

```console
$ kobune status
╭ myapp / feature-auth ──────────────────────────────────╮
│ feature/auth  /path/to/myapp.wt/feature-auth           │
│                                                        │
│ ● web  ready  https://web.feature-auth.myapp.localhost │
│                                                        │
│ shared over the tunnel:                                │
│ web  https://web-feature-auth-myapp.example.com        │
╰────────────────────────────────────────────────────────╯
```

自分のゾーンを使う場合、トンネル側のホスト名は、サービス名・workspace 名・
プロジェクト名を `-` で連結した 1 ラベルになります。ローカルの URL はパート
ごとにサブドメインを分けますが、トンネル側はそうしません。これは証明書の都合
です。Cloudflare の Universal SSL が覆うのは 1 階層目のサブドメインまでで、
それより深いホスト名は TLS のハンドシェイクで拒否されます。トンネルは起動して
いて平文 HTTP なら応答するので、証明書の問題にはまず見えません。1 ラベルなら
無料の証明書の範囲に収まります。

quick のホスト名は Cloudflare 側で決まるもので、Kobune の規則は関与しません。
そのため、どのサービスに届くのかは名前からは分かりません。

`expose = false` のサービスには、どちらの場合もトンネル側のホスト名が割り当て
られません。データベースは、ホスト名を推測されても外部から到達できません。

## 停止中の環境も起動する

レビュアーからの最初のリクエストでも、ローカルと同様に停止中の環境が起動
します。1〜2 秒待つとページが表示されます。

これはルーティングの構造から自然に得られる挙動です。トンネル側のホスト名は
プロキシのルーティングテーブルに `.localhost` と並べて登録され、同じサービスを
参照します。名前を払い出したのがどちらであっても、通常のルートとして扱われ
ます。

## 停止する

```console
$ kobune tunnel disable
╭ tunnel ─────────────────────────────╮
│ disabled  cloudflare  *.example.com │
│                                     │
│ DNS  *.example.com                  │
╰─────────────────────────────────────╯
```

トンネルを停止し、そのホスト名をルーティングテーブルから削除します。quick の
場合はプロセスも終了し、ホスト名も消滅します。named tunnel と DNS レコードは
Cloudflare 側に残ります。維持コストがかからず、残しておけば再開時にログインが
不要になるためです。

## 状態を確認する

```console
$ kobune tunnel status
$ kobune doctor | grep -i tunnel
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

`needs login` は `cloudflare` の状態です。quick はログインを求めないため、
`not installed` から直接 `running` になります。

daemon は再起動時に named tunnel を復元するため、共有した URL はそのまま使用
できます。quick は上記の理由から停止したままになります。

## 構成

2 つは構成が異なります。前掲の表の違いは、すべてここから生じています。

**`cloudflare` は 1 台のマシンにつき 1 本の named tunnel** で、すべての
プロジェクトを中継します。ingress ルールは 1 つのみで、ゾーン全体をローカルの
プロキシへ転送し、Host による振り分けはプロキシが担当します。DNS レコードも
ワイルドカード 1 件（`*.example.com`）のみのため、プロジェクトや worktree が
増減しても DNS の更新も `cloudflared` の再読み込みも発生しません。

**`quick` はサービスごとに `cloudflared` を 1 つ**起動します。ゾーンがないため
ワイルドカードが使えず、ワイルドカードがなければ 1 つのホスト名は 1 つの転送先
にしか届きません。つまり、サービスごとに接続とホスト名が必要になります。どこに
も登録を残さないことが、アカウントを不要にしている理由であり、URL が一時的で
ある理由でもあります。

`cloudflared` からプロキシまでの区間は、どちらもループバック上の平文 HTTP です。
TLS は Cloudflare のエッジで終端されており、また `cloudflared` にローカル CA を
信頼させる理由もないためです。

::: tip 実際のゾーンで確認済みです
ホスト名のルーティング、生成される設定ファイル、CLI に渡す引数、トンネル経由
での自動起動については、スタブを使ったテストで検証しています。加えて `enable`
は実際の Cloudflare ゾーン（Free プラン）に対しても実行済みで、ワイルドカード
レコードが作成され、トンネルの URL がゾーンの Universal SSL 証明書のまま https
で応答することを確認しています。追加の購入も手動設定も必要ありません。
:::
