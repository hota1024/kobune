# `minato.toml`

リポジトリルートに配置し、リポジトリで管理します。すべての worktree が同じ
内容を参照します。

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

| キー | 型 | 既定値 | 説明 |
| --- | --- | --- | --- |
| `name` | string | **必須** | すべての URL に含まれます。1 つの daemon が管理するプロジェクト間で一意である必要があります |
| `domain` | string | `{name}.localhost` | URL の接尾辞。`.localhost` 以外を指定する場合は `/etc/resolver` の設定が別途必要です |

同名のプロジェクトを 2 つ登録しようとした場合は、衝突させずにエラーとします。

## `[runtime]`

| キー | 型 | 既定値 | 説明 |
| --- | --- | --- | --- |
| `default` | string | `"docker"` | `"docker"` または `"apple"` |

[ランタイム](../guide/runtimes) を参照してください。

## `[services.<name>]`

サービス名は URL と `MINATO_URL_<SERVICE>` に含まれるため、英数字と `-` の
範囲に留めてください。

### イメージとコマンド

| キー | 型 | 既定値 | 説明 |
| --- | --- | --- | --- |
| `image` | string | いずれか必須 | 既製イメージ。`postgres:16`、`docker.io/library/node:22` など |
| `build` | string | いずれか必須 | ビルドコンテキスト。worktree からの相対パス。`image` と排他 |
| `dockerfile` | string | `{build}/Dockerfile` | Dockerfile のパス。worktree からの相対。`build` が必要 |
| `build_args` | table | `{}` | `--build-arg` に渡す値。`build` が必要 |
| `command` | string | イメージの既定値 | イメージ側のコマンドを上書きします。シェルと同様に解釈され、引用符で囲んだ範囲は 1 つの引数になります |
| `workdir` | string | `/workspace` | コンテナ内の作業ディレクトリ |

worktree は `/workspace` にマウントされるため、これが既定値になっています。

### ネットワーク

| キー | 型 | 既定値 | 説明 |
| --- | --- | --- | --- |
| `port` | integer | — | アプリケーションが**コンテナ内で**待ち受けるポート |
| `expose` | boolean | `port` があれば `true` | URL を割り当てるかどうか |

ホスト側のポートを設定する項目はありません。Docker は自動的に選択したポートへ
フォワードし、Apple Container はコンテナに専用の IP アドレスを割り当てます。

コンテナ内では `127.0.0.1` ではなく `0.0.0.0` にバインドしてください。
コンテナ内のループバックにバインドしたサーバには、外部から到達できません。

#### イメージをビルドする

```toml
[services.web]
build = "."
dockerfile = "./docker/web.Dockerfile"   # 省略可
build_args = { NODE_VERSION = "22" }     # 省略可
port = 3000
```

コンテキストは **main worktree ではなく、その worktree から**取得します。
Dockerfile を変更したブランチには、その Dockerfile が示すイメージが渡ります。
コンテキストは worktree の内側である必要があり、`build = "../.."` のような
指定は、ビルドコンテキストとしてランタイムに渡す前に拒否されます。

イメージには `minato-{project}-{service}:{fingerprint}` というタグが付きます。
fingerprint は Dockerfile と build_args から算出されます。ここから 2 つの
挙動が導かれます。

- Dockerfile が同一の worktree 同士は同じタグになり、**1 つのイメージを共有**
  します。ビルドは 1 回だけです。
- **そのタグが既にあればビルドをスキップします。** 停止中のサービスを起動する
  際にビルドが走らないのは、この判定によるものです。

::: warning COPY したファイルの変更では再ビルドされません
fingerprint は Dockerfile が `COPY` するファイルまでは見ないため、
`package.json` だけを変更しても再ビルドは発生しません。`minato up --build`
を使用してください。`docker compose` も同様の挙動です。
:::

### 起動完了の判定

| キー | 型 | 既定値 | 説明 |
| --- | --- | --- | --- |
| `health` | string | TCP 接続 | リクエストを受け付けられる状態かどうかの判定方法 |

```toml
health = "http://localhost:3000/healthz"   # 2xx または 3xx
health = "tcp://localhost:5432"            # 接続が成功する
health = "cmd:pg_isready -U postgres"      # コンテナ内で実行
```

`http://` で**使われるのはパスのみ**です。記述するのはコンテナ内から見た
アドレスですが、Minato はランタイムが割り当てたアドレスに接続します。

### ライフサイクル

| キー | 型 | 既定値 | 説明 |
| --- | --- | --- | --- |
| `idle_timeout` | duration | `"30m"` | リクエストが来ない状態が続いたとき、自動停止するまでの時間 |
| `depends_on` | array | `[]` | 先に起動するサービス |
| `scope` | string | `"workspace"` | `"workspace"` または `"project"` |

時間は `humantime` 形式で指定します。`"30s"`、`"10m"`、`"2h"` など。

`depends_on` は起動順序を指定します。Apple Container では、
`MINATO_HOST_<PEER>` が利用可能かどうかもこの指定に依存します。アドレスを
サービスの起動時に取得するためです。

`scope = "project"` は、すべての worktree で 1 インスタンスを共有します。
初期データの投入を繰り返したくないデータベースには適していますが、互換性の
ないマイグレーションを持つブランチが複数ある場合には適しません。

### ストレージ

| キー | 型 | 既定値 | 説明 |
| --- | --- | --- | --- |
| `volumes` | array | `[]` | マウント定義 |

```toml
volumes = [
  "pgdata:/var/lib/postgresql/data",   # 名前付き。worktree 間で共有される
  "./seed:/seed",                      # ホストのパス。worktree からの相対パス
  "/etc/ssl/certs:/certs:ro",          # 絶対パス、読み取り専用
  "~/.cache/npm:/root/.npm",           # ホームディレクトリからの相対パス
]
```

`/` を含まない source は名前付き領域、`/`、`./`、`~/` で始まるものはホストの
パスとして扱われます。末尾の `:ro` / `:rw` でモードを指定でき、既定値は
読み書き可能です。コンテナ側のパスは絶対パスである必要があります。

Apple Container には名前付きボリュームがないため、
`~/.minato/volumes/<project>/` へのバインドマウントに置き換えられます。

### 環境変数

| キー | 型 | 既定値 | 説明 |
| --- | --- | --- | --- |
| `env` | table | `{}` | このサービスに渡す環境変数 |

```toml
env = { NODE_ENV = "development", PORT = "3000" }
```

これは project 層にあたり、リポジトリで管理されます。秘匿すべき値は記述しない
でください。[環境変数](../guide/environment-variables) を参照してください。

## 検証

```console
$ minato status
error: invalid configuration: service `web`: depends_on names an unknown service `database`
```

設定は読み込み時に検証されます。存在しないサービスへの参照、`depends_on` の
循環参照、不正なボリューム指定、不正な時間表記は、いずれもサービスの起動前に
検出されます。
