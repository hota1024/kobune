# Web アプリとデータベース

サービスを 2 つ構成し、そのあと全ブランチで共有する 3 つ目を追加します。
`scope` と `depends_on` が重要になる場面です。

[ブランチごとのプレビュー](./first-preview) の続きです。

## サービスを 2 つ構成する

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
╭ myapp / (main) ───────────────────────────╮
│ main  /path/to/myapp                      │
│                                           │
│ ● web  ready  https://web.myapp.localhost │
│ ● api  ready  https://api.myapp.localhost │
╰───────────────────────────────────────────╯
```

`depends_on` の指定により `api` が先に起動しました。どちらにも URL が
割り当てられています。

## フロントエンドから API を参照する

API の URL はブランチごとに異なるため、ハードコードできません。Minato が
環境変数として注入します。

```js
const api = process.env.MINATO_URL_API   // https://api.feature-x.myapp.localhost
```

```console
$ minato exec web -- printenv MINATO_URL_API
https://api.myapp.localhost
```

すべてのサービスに、他のすべてのサービスの `MINATO_URL_<SERVICE>` が渡されます。
**worktree ごとの環境が成立するのは、この仕組みによるものです。** これがなけ
れば、フロントエンドは URL を推測するほかありません。

同一 workspace 内のサーバ間通信であれば、Docker ではサービス名を直接使用でき
ます（`http://api:8080`）。この場合はプロキシを経由しません。Apple Container
では利用できないため、[ランタイム](../guide/runtimes) を参照してください。

## データベースを追加する

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

ここには 3 つの判断が含まれています。

**`scope = "project"`** — worktree ごとではなく、全体で 1 つのデータベースを
使用します。初期データの投入は 1 回で済み、すべてのブランチが同じデータを
参照します。

**`expose = false`** — URL もルーティングも作成しません。他のサービスからは
到達でき、それ以外からは到達できません。データベースには必ず指定してください。

**`volumes`** — 名前付き領域を使うことで、`down` と `up` を挟んでもデータが
残ります。ホストのパスではなく名前付き領域のため、ランタイムが管理し、
プロジェクト単位で共有されます。

```console
$ minato up
  ✓ starting db
  ✓ starting api
  ✓ starting web
╭ myapp / (main) ───────────────────────────╮
│ main  /path/to/myapp                      │
│                                           │
│ ● web  ready  https://web.myapp.localhost │
│ ● api  ready  https://api.myapp.localhost │
│ ● db   ready  internal only               │
╰───────────────────────────────────────────╯
```

`internal only` は `expose = false` が機能していることを示します。

## 共有されていることを確認する

```console
$ minato new feature/reports
$ cd ../myapp.wt/feature-reports
$ minato status
╭ myapp / feature-reports ──────────────────────────────────╮
│ feature/reports  /path/to/myapp.wt/feature-reports        │
│                                                           │
│ ● web  ready  https://web.feature-reports.myapp.localhost │
│ ● api  ready  https://api.feature-reports.myapp.localhost │
│ ● db   ready  internal only                               │
╰───────────────────────────────────────────────────────────╯
```

`web` と `api` は新しく作成され、`db` は既存のものが使われています。

```console
$ docker ps --filter label=dev.minato.project=myapp --format '{{.Names}}'
minato-myapp-feature-reports-web
minato-myapp-feature-reports-api
minato-myapp-main-web
minato-myapp-main-api
minato-myapp-shared-db
```

`minato-myapp-shared-db` は 1 つだけで、worktree ごとには作成されていません。
一方のブランチで書き込んだデータは、もう一方からも参照できます。

## 共有が適さない場合

互換性のないマイグレーションを持つ 2 つのブランチが 1 つのデータベースを共有
すれば、当然衝突します。Minato はこの問題を解決しません。該当する場合は次の
ように設定します。

```toml
[services.db]
scope = "workspace"   # worktree ごとに 1 つ
```

初期データ投入のコストと引き換えに、ブランチ間の独立性が得られます。この
判断はプロジェクトごとに行ってください。マイグレーションを追加するブランチが
出てきた時点で、方針を見直すことになるケースもあります。

## データベースへの接続設定

```toml
[services.api]
env = { DATABASE_URL = "postgres://postgres:postgres@db:5432/myapp" }
```

Docker では `db:5432` が解決されます。ただし、パスワードはリポジトリに含めない
ほうが望ましいでしょう。

```console
$ minato env set DATABASE_PASSWORD='op://Development/myapp/db' --scope project
```

これは値ではなく参照です。コンテナの起動時に解決され、ディスクには書き込まれ
ません。[環境変数](../guide/environment-variables) を参照してください。

## 複数サービスとアイドルタイムアウト

アクセスとして計測されるのは、**プロキシを経由したリクエストのみ**です。API
からしかアクセスされないデータベースは、API が稼働中でもアイドル状態と判定
され、停止します。

```toml
[services.db]
idle_timeout = "8h"
```

必要になれば起動しますが、この設定により作業中に再起動が繰り返されることを
避けられます。

## 次に読むもの

- [プレビューを共有する](./sharing) — ブランチをインターネットに公開する
- [設定](../guide/configuration) — その他の設定項目
