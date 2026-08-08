# 環境変数

3 つの層を後勝ちで解決し、そのさらに下に Minato が注入する変数があります。

## 層

| 層 | 場所 | コミットする? |
| --- | --- | --- |
| **global** | `~/.minato/env` | しない。マシン固有 |
| **project** | `minato.toml` の `env` と `.minato/env` | する |
| **workspace** | `.minato/env.local` | しない。gitignore する |

後ろが勝ちます。workspace の値が project に勝ち、project が global に勝ちます。

```console
$ minato env ls
DATABASE_URL   project     postgres://db:5432/app
LOG_LEVEL      workspace   debug
API_KEY        global      ****
```

**層は必ず併記されます。** 3 つあるので、いちばん厄介なのは「見ていない層の
値が効いている」状況だからです。

## 設定する

```console
$ minato env set LOG_LEVEL=debug                    # 既定は workspace
$ minato env set DATABASE_URL=… --scope project     # コミットされ、共有される
$ minato env set GITHUB_TOKEN=… --scope global      # すべてのプロジェクト
$ minato env unset LOG_LEVEL
```

ファイルを直接編集せず `minato env` から書いてください。意図した層に置かれ、
書式も揃います。

::: warning 変更には再起動が要ります
すでに動いているコンテナは新しい値を拾いません。
`minato down && minato up` してください。
:::

## Minato が注入する変数

すべてのサービスが以下を受け取ります。あなたの値より下の層なので、どれでも
上書きできます。

```
MINATO_PROJECT      = myapp
MINATO_WORKSPACE    = feature-user-auth
MINATO_SERVICE      = web
MINATO_URL_WEB      = https://web.feature-user-auth.myapp.localhost
MINATO_URL_API      = https://api.feature-user-auth.myapp.localhost
```

重要なのは `MINATO_URL_<SERVICE>` です。worktree ごとの環境が成立するのは
これがあるからで、フロントエンドは API の URL をハードコードできません
—— URL はブランチごとに違うからです。

```js
const api = process.env.MINATO_URL_API ?? 'http://localhost:8080'
```

サービス名の `-` は `_` になります。`api-server` なら
`MINATO_URL_API_SERVER` です。

::: tip URL が無いのはプロキシが動いていないとき
プロキシが待ち受けていないときは、空文字ではなく変数そのものが設定されません。
空文字だと「設定されているのに壊れている」状態になり、変数が無いより
ずっと分かりにくくなります。
:::

Apple Container では、peer の IP アドレスを持つ `MINATO_HOST_<SERVICE>` も
あります。[ランタイム](./runtimes) を参照。

## シークレット

秘密をコミットしないでください。参照を書けば、コンテナ起動時に Minato が
解決します。

```
DATABASE_PASSWORD = op://Development/myapp/password    # 1Password CLI
API_KEY           = keychain://minato/myapp/api-key    # macOS Keychain
STRIPE_KEY        = env://STRIPE_KEY                   # daemon の環境変数
```

解決した値はメモリ上でコンテナに渡り、**ディスクには書かれません。**
`minato env ls` は値ではなく参照を表示します。`--reveal` を付けても同じで、
実体を出すには解決が必要で、それは起動時にしか行わないからです。

### 解決に失敗したとき

daemon は落ちません。たいていは 1Password にサインインしていないだけで、
そのために環境全体が起動しないほうが困ります。そのキーだけ落として警告します。

```
warning: cannot resolve the secret for DATABASE_PASSWORD: cannot reach op
```

アプリは変数が無いことで失敗します。間違った値で動くよりは、はっきりした
失敗です。

## 1 つの値を読む

```console
$ minato env get DATABASE_URL
postgres://db:5432/app
```

スクリプト用に 1 行だけ、装飾なしで出します。`env ls` と違って実際の値が
出るのは、名指しで聞いているからです。

## ファイルで扱う場合

```
~/.minato/env              global
.minato/env                project、コミットする
.minato/env.local          workspace、gitignore する
```

`KEY=value` を 1 行ずつ、`#` がコメントです。`.minato/env.local` は
`.gitignore` に入れてください。
