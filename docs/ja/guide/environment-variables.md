# 環境変数

3 つの層を後勝ちで解決します。さらにその下に、Minato が自動的に注入する変数が
あります。

## 3 つの層

| 層 | 保存先 | コミット対象 |
| --- | --- | --- |
| **global** | `~/.minato/env` | 対象外。マシン固有の設定 |
| **project** | `minato.toml` の `env` と `.minato/env` | 対象 |
| **workspace** | `.minato/env.local` | 対象外。gitignore に追加する |

後に定義された層が優先されます。workspace が project より優先され、project が
global より優先されます。

```console
$ minato env ls
DATABASE_URL   project     postgres://db:5432/app
LOG_LEVEL      workspace   debug
API_KEY        global      ****
```

**定義元の層は常に併記されます。** 層が 3 つあるため、意図しない層の値が
優先されている状況が最も特定しにくいためです。

## 値を設定する

```console
$ minato env set LOG_LEVEL=debug                    # 既定は workspace
$ minato env set DATABASE_URL=… --scope project     # コミット対象、全体で共有
$ minato env set GITHUB_TOKEN=… --scope global      # 全プロジェクト共通
$ minato env unset LOG_LEVEL
```

ファイルを直接編集せず、`minato env` から設定してください。指定した層に確実に
書き込まれ、書式も統一されます。

::: warning 反映には再起動が必要です
すでに稼働しているコンテナは、新しい値を読み込みません。
`minato down && minato up` を実行してください。
:::

## 自動的に注入される変数

すべてのサービスに次の変数が渡されます。利用者が設定する値より下の層に位置する
ため、いずれも上書きできます。

```
MINATO_PROJECT      = myapp
MINATO_WORKSPACE    = feature-user-auth
MINATO_SERVICE      = web
MINATO_URL_WEB      = https://web.feature-user-auth.myapp.localhost
MINATO_URL_API      = https://api.feature-user-auth.myapp.localhost
```

とくに重要なのが `MINATO_URL_<SERVICE>` です。URL はブランチごとに異なるため、
フロントエンドは API の URL をハードコードできません。worktree ごとの環境が
成立するのは、この変数があるためです。

```js
const api = process.env.MINATO_URL_API ?? 'http://localhost:8080'
```

サービス名に含まれる `-` は `_` に変換されます。`api-server` であれば
`MINATO_URL_API_SERVER` になります。

::: tip プロキシが停止している場合
プロキシが待ち受けていないときは、空文字ではなく変数自体が設定されません。
空文字を設定すると「値はあるのに接続できない」状態になり、変数が存在しない
場合よりも原因の特定が困難になるためです。
:::

Apple Container では、これに加えて他サービスの IP アドレスを保持する
`MINATO_HOST_<SERVICE>` が注入されます。[ランタイム](./runtimes) を参照して
ください。

## シークレット

秘匿すべき値はコミットしないでください。参照形式で記述しておけば、コンテナの
起動時に Minato が解決します。

```
DATABASE_PASSWORD = op://Development/myapp/password    # 1Password CLI
API_KEY           = keychain://minato/myapp/api-key    # macOS Keychain
STRIPE_KEY        = env://STRIPE_KEY                   # daemon の環境変数
```

解決された値はメモリ上でコンテナに渡され、**ディスクには書き込まれません。**
`minato env ls` は値ではなく参照を表示します。`--reveal` を指定した場合も
同様です。実際の値を表示するには解決処理が必要ですが、それは起動時にのみ
実行するためです。

### 解決に失敗した場合

daemon は停止しません。多くの場合は 1Password にサインインしていないだけで
あり、それによって環境全体が起動しなくなるほうが問題だからです。該当する
キーのみを除外し、警告を出力します。

```
warning: cannot resolve the secret for DATABASE_PASSWORD: cannot reach op
```

アプリケーションは変数が未設定であることを理由に失敗します。誤った値で
動作し続けるよりも、明確な失敗です。

## 単一の値を取得する

```console
$ minato env get DATABASE_URL
postgres://db:5432/app
```

スクリプトから利用できるよう、値を 1 行だけ出力します。`env ls` と異なり
実際の値が表示されるのは、キーを明示的に指定しているためです。

## ファイルで管理する場合

```
~/.minato/env              global
.minato/env                project、コミット対象
.minato/env.local          workspace、gitignore に追加する
```

書式は 1 行につき `KEY=value` で、`#` から始まる行はコメントです。
`.minato/env.local` は `.gitignore` に追加してください。
