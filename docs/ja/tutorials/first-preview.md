# ブランチごとのプレビュー

小さな Node アプリケーションにプレビュー環境を用意し、2 つのブランチがそれぞれ
別の URL で同時に動作する状態まで構築します。

所要時間は 15 分程度です。Kobune がインストール済みで、`kobune doctor` が
問題なく通ることを前提とします。

## 対象のアプリケーション

ポートで待ち受けるものであれば何でも構いません。用意がなければ、次のものを
使ってください。

```console
$ mkdir myapp && cd myapp && git init
$ npm init -y && npm pkg set scripts.dev="node server.js"
```

```js
// server.js
import { createServer } from 'node:http'

const banner = process.env.BANNER ?? 'hello'

createServer((_, res) => {
  res.writeHead(200, { 'content-type': 'text/plain' })
  res.end(`${banner} from ${process.env.KOBUNE_WORKSPACE ?? 'somewhere'}\n`)
}).listen(3000, '0.0.0.0')
```

```console
$ npm pkg set type=module
$ git add -A && git commit -m "a server"
```

::: warning 0.0.0.0 にバインドしてください
`listen(3000)` のみでも `0.0.0.0` にバインドされます。コンテナ内で
`127.0.0.1` にバインドしたサーバには外部から到達できません。最初によくある
間違いです。
:::

## 設定を作成する

```console
$ kobune init
```

`kobune.toml` を次のように編集します。

```toml
[project]
name = "myapp"

[runtime]
default = "docker"

[services.web]
image = "node:22"
port = 3000
command = "npm run dev"
health = "http://localhost:3000/"
```

`health` は任意ですが、最初から設定しておくことを推奨します。未設定の場合の
判定は TCP 接続の可否のみとなり、アプリケーションが応答可能になる前に成立して
しまいます。

```console
$ git add kobune.toml && git commit -m "kobune"
```

## 起動する

```console
$ kobune up
  ✓ pulling image node:22
  ✓ starting web
  ✓ waiting for web
╭ myapp / (main) ───────────────────────────╮
│ main  /path/to/myapp                      │
│                                           │
│ ● web  ready  https://web.myapp.localhost │
╰───────────────────────────────────────────╯
```

```console
$ curl -sS --fail-with-body https://web.myapp.localhost
hello from main
```

`KOBUNE_WORKSPACE` が注入されているため、アプリケーションは自身がどのブランチ
で動作しているかを把握できます。

## ブランチを作成する

```console
$ kobune new feature/loud-banner
  ✓ creating worktree feature/loud-banner
  ✓ starting web
╭ myapp / feature-loud-banner ──────────────────────────────────╮
│ feature/loud-banner  /path/to/myapp.wt/feature-loud-banner    │
│                                                               │
│ ● web  ready  https://web.feature-loud-banner.myapp.localhost │
╰───────────────────────────────────────────────────────────────╯
```

環境が 2 つになりました。既存の環境は停止しておらず、ポート番号の指定も
不要です。

## ブランチ側にだけ変更を加える

```console
$ cd ../myapp.wt/feature-loud-banner
$ kobune env set BANNER=HELLO
$ kobune down && kobune up
```

```console
$ curl -sS https://web.feature-loud-banner.myapp.localhost
HELLO from feature-loud-banner

$ curl -sS https://web.myapp.localhost
hello from main
```

設定した値は workspace 層に保存されるため、この worktree にのみ適用されます。

```console
$ kobune env ls
╭ environment ─────────────╮
│ KEY     SCOPE      VALUE │
│ BANNER  workspace  HELLO │
╰──────────────────────────╯
```

`down && up` が必要だったのは、稼働中のコンテナが新しい値を読み込まないため
です。

## 自動停止と自動起動を確認する

```console
$ kobune down
$ kobune status
╭ myapp / feature-loud-banner ────────────────────────────────────╮
│ feature/loud-banner  /path/to/myapp.wt/feature-loud-banner      │
│                                                                 │
│ ○ web  stopped  https://web.feature-loud-banner.myapp.localhost │
╰─────────────────────────────────────────────────────────────────╯
```

停止しても URL は残ります。

```console
$ time curl -sS https://web.feature-loud-banner.myapp.localhost
HELLO from feature-loud-banner
curl …  0.01s user … 2.104s total
```

2 秒で起動しました。作業中のブランチに対して `kobune up` を再実行する必要は
ありません。放置した worktree がリソースを消費しないため、必要なだけ作成でき
ます。

## コンテナ内を確認する

```console
$ kobune logs web -n 20
$ kobune exec web -- node --version
v22.14.0
$ kobune exec web -- npm test; echo $?
```

終了コードは実行したコマンドのものが返るため、`npm test` の結果でスクリプトを
分岐できます。

## 後片付け

```console
$ cd ../../myapp
$ kobune rm -w feature-loud-banner
$ kobune ls
╭ workspaces ─────────────────╮
│ WORKSPACE  SERVICES  BRANCH │
│ (main)     1/1       main   │
╰─────────────────────────────╯
```

ブランチは残っています。削除されたのは worktree とコンテナのみです。

## 次に読むもの

- [Web アプリとデータベース](./multi-service) — 複数サービスと、ブランチ間で
  共有するサービス
- [基本操作](../guide/workflow)
