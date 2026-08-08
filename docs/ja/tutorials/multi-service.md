# Web アプリとデータベース

サービスを 2 つ、そのあとすべてのブランチで共有する 3 つ目を足します。
`scope` と `depends_on` が効いてくるところです。

[ブランチごとのプレビュー](./first-preview) の続きです。

## サービス 2 つ

```toml
[project]
name = "myapp"

[runtime]
default = "docker"

[services.web]
image = "node:22"
port = 3000
command = "npm run dev"
depends_on = ["api"]

[services.api]
image = "node:22"
port = 8080
command = "npm run api"
health = "http://localhost:8080/healthz"
```

```console
$ minato up
  ✓ starting api
  ✓ waiting for api
  ✓ starting web
  ✓ waiting for web

  web   ready     https://web.myapp.localhost
  api   ready     https://api.myapp.localhost
```

`depends_on` で `api` が先に起動しました。どちらにも URL が生えています。

## フロントエンドに API を見つけさせる

API の URL はブランチごとに違うので、ハードコードできません。Minato が
注入します。

```js
const api = process.env.MINATO_URL_API   // https://api.feature-x.myapp.localhost
```

```console
$ minato exec web -- printenv MINATO_URL_API
https://api.myapp.localhost
```

すべてのサービスが、他のすべてのサービスの `MINATO_URL_<SERVICE>` を受け取り
ます。**worktree ごとの環境が成立するのはこれのおかげ** で、無ければ
フロントエンドは推測するしかありません。

同じ workspace 内のサーバ間通信なら、Docker ではサービス名を直接使えます
（`http://api:8080`）。プロキシを経由しません。Apple Container では動きません。
[ランタイム](../guide/runtimes) を参照。

## データベースを足す

```toml
[services.db]
image = "postgres:16"
port = 5432
scope = "project"
expose = false
volumes = ["pgdata:/var/lib/postgresql/data"]
env = { POSTGRES_PASSWORD = "postgres", POSTGRES_DB = "myapp" }

[services.api]
image = "node:22"
port = 8080
command = "npm run api"
depends_on = ["db"]
```

3 つの判断が入っています。

**`scope = "project"`** — worktree ごとに 1 つではなく、全体で 1 つの
データベース。seed は一度で済み、ブランチは同じデータを見ます。

**`expose = false`** — URL もルートも無し。他のサービスからは届き、それ以外
からは届きません。データベースには必ず付けてください。

**`volumes`** — 名前付き領域なので `down` / `up` を挟んでもデータが残ります。
ホストのパスではなく名前付きなので、ランタイムが管理し、プロジェクト単位に
なります。

```console
$ minato up
  ✓ starting db
  ✓ starting api
  ✓ starting web

  web   ready     https://web.myapp.localhost
  api   ready     https://api.myapp.localhost
  db    ready     (internal only)
```

`(internal only)` が `expose = false` の効果です。

## 共有されているか確かめる

```console
$ minato new feature/reports
$ cd ../myapp.wt/feature-reports
$ minato status
  web   ready     https://web.feature-reports.myapp.localhost
  api   ready     https://api.feature-reports.myapp.localhost
  db    ready     (internal only)
```

`web` と `api` は新しく、`db` は *同じもの* です。

```console
$ docker ps --filter label=dev.minato.project=myapp --format '{{.Names}}'
minato-myapp-feature-reports-web
minato-myapp-feature-reports-api
minato-myapp-main-web
minato-myapp-main-api
minato-myapp-shared-db
```

`minato-myapp-shared-db` が 1 つだけ、worktree ごとではありません。片方の
ブランチで書いた行が、もう片方から見えます。

## 共有が向かないとき

互換性のないマイグレーションを持つ 2 つのブランチが 1 つのデータベースを
共有すれば、ぶつかります。Minato はこれを解決しません。当てはまるときは:

```toml
[services.db]
scope = "workspace"   # 1 つずつ
```

seed のコストを払って独立を得ます。プロジェクトごとに決め、マイグレーションを
足すブランチで考えが変わることを見込んでおいてください。

## 接続する

```toml
[services.api]
env = { DATABASE_URL = "postgres://postgres:postgres@db:5432/myapp" }
```

Docker では `db:5432` が解決します。より良いのは、パスワードをリポジトリから
出すことです。

```console
$ minato env set DATABASE_PASSWORD='op://Development/myapp/db' --scope project
```

これは値ではなく参照です。コンテナ起動時に解決され、ディスクには書かれません。
[環境変数](../guide/environment-variables) を参照。

## 複数サービスとアイドルタイムアウト

活動として数えるのは **プロキシを通ったリクエスト** だけです。API しか
話しかけないデータベースは、API が忙しくてもアイドルに見えて止まります。

```toml
[services.db]
idle_timeout = "8h"
```

必要になれば起こされますが、1 日の作業中に再起動を繰り返さずに済みます。

## 次に

- [プレビューを共有する](./sharing) — ブランチをインターネットに置く
- [設定](../guide/configuration) — 残りのキー
