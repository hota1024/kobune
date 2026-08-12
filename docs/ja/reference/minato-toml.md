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
| `carry` | array | `[]` | 新しい worktree にコピーするファイル。リポジトリルートからの相対パス |

同名のプロジェクトを 2 つ登録しようとした場合は、衝突させずにエラーとします。

### `carry`

```toml
[project]
carry = [".env", "apps/api/.dev.vars"]
```

`git worktree add` が新しい worktree に用意するのは追跡対象のファイルだけ
です。そのため、追跡対象外でありながら必須である `.env` などは存在せず、
サービスが起動できません。ここに列挙したファイルは `minato new` の際に
メインの worktree からコピーされ、サービスの起動前に配置されます。

- **コピー元が無いことはエラーではありません。** すべてのチェックアウトに
  `.env` があるとは限らず、無いことを理由に `minato new` を失敗させるのは
  埋めようとしている穴より悪い結果になります。黙って無視はせず報告します。
- **コピー先が既に存在する場合は上書きしません。** git がチェックアウトした
  内容が優先されます。これは git が持ってこないものを補う仕組みであって、
  git が持ってくるものを置き換える仕組みではありません。
- パーミッションも引き継がれるため、`0600` の `.env` は `0600` のままです。
- リポジトリの外に出るパスは、シンボリックリンク経由も含めて拒否されます。
  ディレクトリはコピーされないので、ファイルを個別に指定してください。

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
| `setup` | string | — | サービスの初回起動前に一度だけ実行されます。シェルと同様に解釈されます |
| `workdir` | string | `/workspace` | コンテナ内の作業ディレクトリ |
| `tty` | bool | `false` | プロセスを端末上で動かし、標準入力を開いたままにします |

worktree は `/workspace` にマウントされるため、これが既定値になっています。

#### `tty`

```toml
[services.dev]
image = "node:24-bookworm-slim"
command = "npx turbo run dev"
tty = true
```

プログラムが描画を始める前に確かめているのがこれです。Turborepo や Vitest
などは「相手が端末かどうか」を尋ね、端末でなければただ流れるテキストで妥協し
ます。これがない状態のコンテナはまさにそれにあたります。有効にすると色が通り、
[`minato logs -f dev`](./cli#サービスに入力する) がその端末になります。つまり
入力がプログラムに届きます。

::: warning 端末はログの性質そのものを変えます
出力ストリームが 1 本にまとまるため stderr と stdout の区別がなくなり、行末は
`\r\n` になります。これは Minato が足しているものではなく、端末とはそういう
ものだからです。ログをパイプで処理するサービスでは `tty` を切ったままにして
ください。
:::

コンテナが端末を持つかどうかは作成時に決まります。そのため、すでに起動している
サービスでこれを有効にすると、次の `minato up` でコンテナが作り直されます
（再起動になります）。

#### `setup`

```toml
[services.web]
image = "node:24-bookworm-slim"
setup = "sh -c 'pnpm install --frozen-lockfile'"
command = "sh -c 'pnpm dev'"
```

サービスの初回起動前に実行されるため、`command` は「アプリを起動するだけ」に
できます。実行されるのはサービスのイメージ・環境変数・ボリュームを備えた専用の
コンテナなので、ボリュームに導入した内容は本来のコンテナ起動時に揃っています。

**コンテナごとではなく worktree ごとに 1 回です。** 停止中のコンテナは次の
`up` で作り直されるため、コンテナ作成に紐づけると `down` / `up` のたびに実行
されてしまい、この機能が避けようとしている状態そのものになります。Minato は
実行したコマンドを worktree に対して記録します。

- `setup` の内容を変更すると再実行されます。比較対象は内容そのものなので、
  再実行したいときは書き換えてください。**`image` の変更では再実行されません**。
  古いランタイム向けにビルドされたネイティブモジュールはボリュームに残ります
- 失敗した `setup` は `up` を中断し、記録もされません。修正して `up` し直せば
  再試行されます
- `minato rm` は記録を消します。`@workspace` ボリュームも一緒に消えます
- `scope = "project"` のサービスは worktree ごとではなくプロジェクトで 1 回です。
  コンテナがすべての worktree で 1 つだからです

実行されるのは `startup_order` の中、そのサービス自身が起動する直前です。
そのため `depends_on` に挙げたサービスは既に起動しています。`db` に対する
マイグレーションは動作しますが、**自分自身のサービスが起動していることを前提と
する `setup`** は動作しません。これから起動するものだからです。

**`setup` は同時に 1 つしか実行されません。** 周囲のサービスが同時に起動する
場合でも同じです。すべてのサービスはプロジェクト共通のキャッシュボリュームを
マウントしているため、2 つの `setup` を同時に走らせることは、任意のコマンド
2 つが 1 つのディレクトリに書き込むことを意味します。パッケージマネージャの
ストアはそのために作られているので安全ですが、`setup` が他に何をするかまで
Minato は保証できません。そのため直列にしています。`minato new` 後の初回 `up`
は setup の時間を素直に足し合わせただけかかり、2 回目以降は記録済みとして
読み飛ばされます。

停止中のサービスがリクエストで起こされる際に `setup` は実行されません。実行
されるのは `minato up` のときだけなので、編集内容は次のリクエストではなく次の
`up` で反映されます。

ホスト側の特権設定を行う `minato setup` とは別のものです。

### ネットワーク

| キー | 型 | 既定値 | 説明 |
| --- | --- | --- | --- |
| `port` | integer | — | アプリケーションが**コンテナ内で**待ち受けるポート |
| `expose` | boolean | `port` があれば `true` | URL を割り当てるかどうか。割り当てない場合、起動と停止は `depends_on` 経由になる |

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

サービスの起動時にはこの判定が通るまで待機します。`minato up` の直後の
`curl` が connection refused にならないのはこのためです。

::: warning 待機は 15 秒で打ち切られます
無制限に待つと `minato up` が返らなくなるため、15 秒で待機をやめて先に
進みます。初回起動でコンパイルに 1 分かかる dev server などはこれに該当
します。URL 自体はアクセス時に待機するので壊れてはいませんが、その時点で
`depends_on` は保証ではなくなります。
:::

### ライフサイクル

| キー | 型 | 既定値 | 説明 |
| --- | --- | --- | --- |
| `idle_timeout` | duration | `"30m"` | リクエストが来ない状態が続いたとき、自動停止するまでの時間。URL を持たないサービスは `depends_on` で参照している側に従う |
| `depends_on` | array | `[]` | 先に起動するサービス。`minato up` でも、リクエストによる起動でも先に起動する |
| `scope` | string | `"workspace"` | `"workspace"` または `"project"` |

時間は `humantime` 形式で指定します。`"30s"`、`"10m"`、`"2h"` など。

`depends_on` は依存先を先に起動し、**ready になるまで待機します**。判定
方法は [起動完了の判定](#起動完了の判定) と同じで、そこに記載した 15 秒の
上限も同様に適用されます。Apple Container では、`MINATO_HOST_<PEER>` が
利用可能かどうかもこの指定に依存します。アドレスをサービスの起動時に取得
するためです。

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
  "node-modules@workspace:/workspace/node_modules",  # worktree ごとに分離
  "./seed:/seed",                      # ホストのパス。worktree からの相対パス
  "/etc/ssl/certs:/certs:ro",          # 絶対パス、読み取り専用
  "~/.cache/npm:/root/.npm",           # ホームディレクトリからの相対パス
]
```

`/` を含まない source は名前付き領域、`/`、`./`、`~/` で始まるものはホストの
パスとして扱われます。末尾の `:ro` / `:rw` でモードを指定でき、既定値は
読み書き可能です。コンテナ側のパスは絶対パスである必要があります。

**名前付き領域はプロジェクト単位で名前空間が切られています。** `pgdata` は
Docker のボリューム `minato-{project}-pgdata` になるため、自分で prefix を
付ける必要はありません。プロジェクト `myapp` で `myapp-pgdata` と書くと
`minato-myapp-myapp-pgdata` になります。

#### スコープ

名前付き領域は、既定ではプロジェクト内のすべての worktree で共有されます。
パッケージキャッシュに向いているのはこのためであり、ブランチによって内容が
変わるものに向かないのもこのためです。ブランチごとに lockfile が異なる
`node_modules` を共有すると壊れます。

名前に `@workspace` を付けると、worktree ごとに分離されます。

```toml
volumes = [
  "pnpm-store:/pnpm-store",                          # 共有
  "node-modules@workspace:/workspace/node_modules",  # worktree ごと
  "certs@workspace:/certs:ro",                       # :ro と併用できる
]
```

| 記述 | 実際の Docker ボリューム名 |
| --- | --- |
| `pnpm-store` | `minato-{project}-pnpm-store` |
| `node-modules@workspace` | `minato-{project}-{workspace}.node-modules` |

worktree 名の連結に `-` ではなく `.` を使っているのは意図的です。project、
worktree、ボリューム名はいずれも DNS ラベルであり、どれにもハイフンが
含まれ得ます。`-` で連結すると、worktree `feat-1` のボリューム `cache` と、
project スコープのボリューム `feat-1-cache` が同一の領域になってしまいます。
`.` はラベルに含められないため、両者が衝突することはありません。

ボリューム名自体もラベルである必要があります（英小文字・数字・ハイフン）。

明示したい場合は `@project` と書けますが、省略時の既定値も同じです。認識
できない接尾辞はエラーになります。`@worktree` のような打ち間違いを名前の
一部として受け入れると、`node-modules@worktree` という共有ボリュームが
黙って作られてしまうためです。

**workspace スコープのボリュームは worktree と一緒に削除されます。**
`minato rm` がコンテナとあわせて削除します。所属する worktree が無くなる
以上、残しても到達できないためです。project スコープのボリュームは共有物で
あり個々の worktree より長生きするため、削除されません。

したがって project スコープのボリュームを削除するコマンドは
[`minato uninstall`](./cli#アンインストール) だけで、見つけたものをすべて
一覧に出してから確認を求めます。個別に消したい場合は実体を消してください。
`docker volume rm minato-{project}-{name}`、Apple Container なら
`~/.minato/volumes/` 以下のディレクトリです。

::: warning 既存ボリュームのスコープ変更について
スコープは実際のボリューム名の一部です。そのため `@workspace` の付け外しは
参照先のストレージそのものを変えます。削除はされませんが、それまでの内容は
見えなくなります。
:::

`scope = "project"` のサービスは `@workspace` の領域を要求できません。
1 つのインスタンスがすべての worktree に対応する以上、どの worktree のもの
かを決められないためです。これは設定の読み込み時にエラーとして検出されます。

Apple Container には名前付きボリュームがないため、
`~/.minato/volumes/<project>/` へのバインドマウントに置き換えられます。

### 環境変数

| キー | 型 | 既定値 | 説明 |
| --- | --- | --- | --- |
| `env` | table | `{}` | このサービスに渡す環境変数 |
| `env_file` | string | — | 解決済みの環境変数の書き出し先。worktree からの相対パス |

```toml
env = { NODE_ENV = "development", PORT = "3000" }
```

値の中では他の変数を参照できます。worktree ごとに変わる URL を、アプリケー
ションが既に読んでいる名前で渡すための書き方です。

```toml
env = { NEXT_PUBLIC_API_URL = "${MINATO_URL_API}" }
```

これは project 層にあたり、リポジトリで管理されます。秘匿すべき値は記述しない
でください。[環境変数](../guide/environment-variables) を参照してください。

`env_file` は解決結果を、自身の環境変数ではなくファイルを読む道具のために
書き出します。

```toml
env_file = ".minato/env.api"
```

書き込まれるのは起動するサービスの分だけで、そのサービスの起動直前です。
シークレットは含まれません。git が追跡
しているパスは拒否され、Minato が書いたのでないファイルは上書きされません。
Minato 自身が層として読む `.minato/env` と `.minato/env.local`、および他の
サービスが既に使っているパスも拒否されます。また `scope = "project"` の
サービスでは使えません。書き込む先の worktree がマウントされていないためです。

## 検証

```console
$ minato status
✗ error: invalid configuration: service `web`: depends_on names an unknown service `database`
```

設定は読み込み時に検証されます。存在しないサービスへの参照、`depends_on` の
循環参照、不正なボリューム指定、リポジトリの外に出る `carry` の指定、不正な
時間表記は、いずれもサービスの起動前に検出されます。
