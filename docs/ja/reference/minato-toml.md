# `minato.toml`

リポジトリルートに置き、コミットします。すべての worktree が同じものを
読みます。

```toml
[project]
name = "myapp"
# domain = "myapp.localhost"

[runtime]
default = "docker"

[services.web]
image = "node:22"
port = 3000
command = "npm run dev"
health = "http://localhost:3000/healthz"
idle_timeout = "30m"
depends_on = ["db"]
env = { NODE_ENV = "development" }

[services.db]
image = "postgres:16"
port = 5432
scope = "project"
expose = false
volumes = ["pgdata:/var/lib/postgresql/data"]
```

## `[project]`

| キー | 型 | | |
| --- | --- | --- | --- |
| `name` | string | **必須** | すべての URL に現れます。1 つの daemon が管理するプロジェクト内で一意である必要があります |
| `domain` | string | `{name}.localhost` | URL の接尾辞。`.localhost` 以外は `/etc/resolver` の設定が別途必要です |

同じ名前で 2 つ登録しようとすると、衝突させる代わりに拒否されます。

## `[runtime]`

| キー | 型 | | |
| --- | --- | --- | --- |
| `default` | string | `"docker"` | `"docker"` または `"apple"` |

[ランタイム](../guide/runtimes) を参照。

## `[services.<name>]`

サービス名は URL と `MINATO_URL_<SERVICE>` に現れるので、英数字と `-` に
留めてください。

### イメージとコマンド

| キー | 型 | | |
| --- | --- | --- | --- |
| `image` | string | **必須** | 既製イメージ。`postgres:16`、`docker.io/library/node:22` |
| `build` | string | — | **未対応。** Dockerfile のビルド用に予約 |
| `command` | string | イメージの既定 | イメージのコマンドを置き換えます。シェル風に解釈され、引用符は 1 引数にまとまります |
| `workdir` | string | `/workspace` | コンテナ内の作業ディレクトリ |

worktree は `/workspace` にマウントされるので、それが既定になっています。

### ネットワーク

| キー | 型 | | |
| --- | --- | --- | --- |
| `port` | integer | — | アプリが **コンテナの中で** 待ち受けるポート |
| `expose` | boolean | `port` があれば `true` | URL を生やすかどうか |

ホスト側のポートを設定する場所はありません。Docker は自分で選んだポートに
フォワードし、Apple Container はコンテナに自分の IP を与えます。

コンテナの中では `127.0.0.1` ではなく `0.0.0.0` に bind してください。
コンテナ内のループバックに bind したサーバは、外から届きません。

### 起動完了の判定

| キー | 型 | | |
| --- | --- | --- | --- |
| `health` | string | TCP 接続 | 受け付け可能かどうかの判定方法 |

```toml
health = "http://localhost:3000/healthz"   # 2xx または 3xx
health = "tcp://localhost:5432"            # 接続が通る
health = "cmd:pg_isready"                  # 未対応
```

`http://` では **パスだけが使われます。** 書くのはコンテナの中から見た
アドレスで、Minato はランタイムが割り当てたアドレスに届きます。

### ライフサイクル

| キー | 型 | | |
| --- | --- | --- | --- |
| `idle_timeout` | duration | `"30m"` | リクエストが来ないまま自分を止めるまでの時間 |
| `depends_on` | array | `[]` | 先に起動するサービス |
| `scope` | string | `"workspace"` | `"workspace"` または `"project"` |

時間は `humantime` 形式です。`"30s"`、`"10m"`、`"2h"`。

`depends_on` は順序を決めます。Apple Container では、`MINATO_HOST_<PEER>` が
使えるかどうかもこれで決まります。アドレスはサービス起動時に読むためです。

`scope = "project"` はすべての worktree で 1 インスタンスを共有します。何度も
seed したくないデータベースには向き、互換性のないマイグレーションを持つ
ブランチが 2 つあるときには向きません。

### ストレージ

| キー | 型 | | |
| --- | --- | --- | --- |
| `volumes` | array | `[]` | マウント |

```toml
volumes = [
  "pgdata:/var/lib/postgresql/data",   # 名前付き。worktree 間で共有
  "./seed:/seed",                      # ホストのパス。worktree からの相対
  "/etc/ssl/certs:/certs:ro",          # 絶対パス、読み取り専用
  "~/.cache/npm:/root/.npm",           # ホームからの相対
]
```

`/` を含まない source は名前付き領域、`/` `./` `~/` で始まるものはホストの
パスです。末尾の `:ro` / `:rw` でモードを指定し、既定は読み書き可です。
コンテナ側のパスは絶対パスである必要があります。

Apple Container に名前付きボリュームは無いので、
`~/.minato/volumes/<project>/` の bind mount になります。

### 環境変数

| キー | 型 | | |
| --- | --- | --- | --- |
| `env` | table | `{}` | このサービスの変数 |

```toml
env = { NODE_ENV = "development", PORT = "3000" }
```

これは *project* 層で、コミットされます。シークレットは入れないでください。
[環境変数](../guide/environment-variables) を参照。

## 検証

```console
$ minato status
error: invalid configuration: service `web`: depends_on names an unknown service `database`
```

設定は読み込み時に検証されます。存在しないサービスの参照、`depends_on` の
循環、不正なボリューム指定、不正な時間表記は、何かが起動する前に見つかります。
