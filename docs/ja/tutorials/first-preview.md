# ブランチごとのプレビュー

小さな Node アプリにプレビュー環境を与え、2 つのブランチがそれぞれの URL で
並んで動いている状態まで持っていきます。

15 分ほど。Minato がインストールされ、`minato doctor` が通っている前提です。

## アプリ

ポートで待ち受けるものなら何でも構いません。手元に無ければ:

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
  res.end(`${banner} from ${process.env.MINATO_WORKSPACE ?? 'somewhere'}\n`)
}).listen(3000, '0.0.0.0')
```

```console
$ npm pkg set type=module
$ git add -A && git commit -m "a server"
```

::: warning 0.0.0.0 に bind すること
`listen(3000)` だけでも `0.0.0.0` になり、それが望みです。コンテナの中で
`127.0.0.1` に bind したサーバには外から届きません。最初によくやる間違いです。
:::

## 設定を書く

```console
$ minato init
```

`minato.toml` を編集します。

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

`health` は任意ですが、最初から書いておく価値があります。無いと判定は
「TCP 接続が通った」だけになり、アプリが応答できる前に true になり得ます。

```console
$ git add minato.toml && git commit -m "minato"
```

## 起動する

```console
$ minato up
  ✓ pulling image node:22
  ✓ starting web
  ✓ waiting for web

myapp / (main)  (main)
  web   ready     https://web.myapp.localhost
```

```console
$ curl -sS --fail-with-body https://web.myapp.localhost
hello from main
```

`MINATO_WORKSPACE` が注入されていて、アプリは自分がどのブランチか知っています。

## ブランチを切る

```console
$ minato new feature/loud-banner
  ✓ creating worktree feature/loud-banner
  ✓ starting web
  web   ready     https://web.feature-loud-banner.myapp.localhost
```

環境が 2 つになりました。何も止まらず、ポートも選んでいません。

## ブランチにだけ変更を入れる

```console
$ cd ../myapp.wt/feature-loud-banner
$ minato env set BANNER=HELLO
$ minato down && minato up
```

```console
$ curl -sS https://web.feature-loud-banner.myapp.localhost
HELLO from feature-loud-banner

$ curl -sS https://web.myapp.localhost
hello from main
```

値は *workspace* 層に入ったので、この worktree にだけ効きます。

```console
$ minato env ls
BANNER   workspace   HELLO
```

`down && up` が必要だったのは、動いているコンテナが新しい値を拾わないから
です。

## 止まって、また起きるのを見る

```console
$ minato down
$ minato status
  web   stopped   https://web.feature-loud-banner.myapp.localhost
```

URL は残っています。停止は消滅ではありません。

```console
$ time curl -sS https://web.feature-loud-banner.myapp.localhost
HELLO from feature-loud-banner
curl …  0.01s user … 2.104s total
```

2 秒で上がりました。まだ使っているブランチに `minato up` を打ち直す必要は
ありません。放置した worktree が何も食わないからこそ、気軽に作れます。

## 中を見る

```console
$ minato logs web -n 20
$ minato exec web -- node --version
v22.14.0
$ minato exec web -- npm test; echo $?
```

終了コードはコマンドのものなので、`npm test` でスクリプトを分岐できます。

## 片付ける

```console
$ cd ../../myapp
$ minato rm -w feature-loud-banner
$ minato ls
WORKSPACE   SERVICES   BRANCH
(main)      1/1        main
```

ブランチは残っています。消えたのは worktree とコンテナだけです。

## 次に

- [Web アプリとデータベース](./multi-service) — 複数サービスと、ブランチ間で
  共有するもの
- [日々の使い方](../guide/workflow)
