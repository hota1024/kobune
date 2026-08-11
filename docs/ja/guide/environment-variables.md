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
╭ environment ────────────────────────────────────╮
│ KEY           SCOPE      VALUE                  │
│ DATABASE_URL  project    postgres://db:5432/app │
│ LOG_LEVEL     workspace  debug                  │
│ API_KEY       global     ****                   │
╰─────────────────────────────────────────────────╯
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
MINATO_CACHE_DIR    = /var/cache/minato
MINATO_URL_WEB      = https://web.feature-user-auth.myapp.localhost
MINATO_URL_API      = https://api.feature-user-auth.myapp.localhost
MINATO_HOSTNAME_WEB = web.feature-user-auth.myapp.localhost
MINATO_HOSTNAME_API = api.feature-user-auth.myapp.localhost
```

### `MINATO_CACHE_DIR`

残す価値はあるがコミットする必要はないものの置き場所です。Minato が管理する
ボリュームで、すべてのサービスにマウントされます。

```toml
[services.web.env]
npm_config_store_dir = "${MINATO_CACHE_DIR}/pnpm"
CARGO_HOME = "${MINATO_CACHE_DIR}/cargo"
```

::: warning 波括弧は省略できません
`${MINATO_CACHE_DIR}` は[参照](#他の変数を参照する)であり、Minato が展開します。
波括弧の無い `$MINATO_CACHE_DIR` は書いたまま渡され、Docker も展開しません。
`npm_config_store_dir = "$MINATO_CACHE_DIR/pnpm"` と書くと、workdir すなわち
worktree からの相対パスとして `$MINATO_CACHE_DIR` という名前のディレクトリが
作られます。これはまさに、この仕組みが防ごうとしている「リポジトリ内に数 GB」
そのものです。この書き方をした値には `minato up` が警告します。

シェルが展開する場所——`command` や起動スクリプト——では波括弧は不要です。

```toml
command = "sh -c 'pnpm config set store-dir $MINATO_CACHE_DIR/pnpm && pnpm dev'"
```
:::

**パッケージマネージャの参照先をここに向けてください。** 既定のままでは多くが
作業ディレクトリ配下にキャッシュを作りますが、そこは worktree、つまりホストから
バインドマウントされた領域です。結果としてキャッシュがリポジトリの中に生まれ、
pnpm の store であれば数 GB の追跡対象外ファイルがチェックアウトに残ります。

プロジェクト内のすべての worktree で共有されます。パッケージの取得を 1 回で
済ませるのが目的だからです。ブランチによって内容が変わるもの（ブランチごとに
lockfile が異なる `node_modules` など）には
[`@workspace` ボリューム](../reference/minato-toml#スコープ) を使ってください。

::: warning root 以外で動作するコンテナの場合
ボリュームは空かつ root 所有で作成されるため、別のユーザーで動作するサービスは
自分が所有するディレクトリが作られるまで書き込めません。インストール処理だけ
`USER root` にするか、起動スクリプトで
`mkdir -p "$MINATO_CACHE_DIR/x" && chown` してください。
:::

コンテナは作成時のマウント構成を保持するため、アップグレード時にすでに起動して
いたサービスには `minato down && minato up` するまで反映されません。また
`/var/cache/minato` に自前のボリュームをマウントすることはできません。1 つの
パスへの二重マウントはコンテナエンジンのエラーになり、原因となった記述から
遠い場所で表面化するためです。

`minato env ls` が表示するのは全サービスに共通する内容だけです。
`MINATO_SERVICE` とサービス固有の `env` は
`minato env ls --service <name>` で確認してください。

### `MINATO_URL_<SERVICE>`

**とくに重要な変数です。** URL はブランチごとに異なるため、フロントエンドは
API の URL をハードコードできません。worktree ごとの環境が成立するのは、この
変数があるためです。

```js
const api = process.env.MINATO_URL_API ?? 'http://localhost:8080'
```

サービス名に含まれる `-` は `_` に変換されます。`api-server` であれば
`MINATO_URL_API_SERVER` になります。

::: tip プロキシが停止している場合
プロキシが待ち受けていないときは、空文字ではなく変数自体が設定されません。
空文字を設定すると「値はあるのに接続できない」状態になり、変数が存在しない
場合よりも原因の特定が困難になるためです。

コンテナ側では `MINATO_URL_WEB: parameter not set` として現れますが、この
メッセージからここに辿り着く手がかりはありません。プロキシが無い状態で
サービスを起動した場合は `minato up` が警告し、対処方法は `minato doctor`
が示します。
:::

### `MINATO_HOSTNAME_<SERVICE>`

同じホストを、周りに何も付けずに渡します。スキームもポートも末尾のスラッシュも
ありません。

```toml
[services.web.env]
NEXT_ALLOWED_DEV_ORIGIN = "${MINATO_HOSTNAME_WEB}"

[services.api.env]
COOKIE_DOMAIN = "${MINATO_HOSTNAME_API}"
```

**CORS の origin、`allowedDevOrigins`、cookie の domain はいずれも URL ではなく
これを要求します。** この変数が無いと、`MINATO_URL_<SERVICE>` から `sed` で
スキームを削ぎ落とす処理がプロジェクト側に生まれます。

注入される条件は URL と同じです。プロキシが待ち受けている間、かつ URL を公開
しているサービスに限ります。応答しないホスト名を渡すのは、URL 側で避けている
「値はあるのに繋がらない」と同じ状態だからです。

::: warning `MINATO_HOST_<SERVICE>` とは別物です
そちらは Apple Container のもので、他サービスの IP アドレスを保持します。
[ランタイム](./runtimes) を参照してください。
:::

## 他の変数を参照する

値の中の `${NAME}` は、`NAME` の解決結果に置き換えられます。

```toml
[services.web.env]
NEXT_PUBLIC_WEB_URL = "${MINATO_URL_WEB}"
NEXT_PUBLIC_API_URL = "${MINATO_URL_API}"
FILE_BASE_URL       = "${MINATO_URL_API}/dev/r2"
```

**worktree ごとに変わる URL を、アプリケーションが既に読んでいる名前で渡すため
の仕組みです。** `MINATO_URL_API` は Minato の名前で届くため、これを書けないと、
変数を別の変数に写すためだけの起動スクリプトがどのプロジェクトにも生まれます。

参照が解決するのは、どの層が優先されたかを問わず、コンテナに実際に渡る値です。
したがって `.minato/env.local` で `MINATO_URL_API` を上書きすれば、そこから
組み立てられる値もまとめて変わります。参照は連鎖できます。展開後の値は
`minato env ls` にも表示されます。展開前の値の一覧は、どこでも動いていない
ものの一覧だからです。

- **波括弧の無い `$NAME` は展開されません。** これらの値はこれまで書いたまま
  渡されてきたため、いま展開を始めると既存の設定の意味が変わってしまいます。
  存在する変数名がこの形で書かれている場合は `minato up` が警告するので、
  症状から探し当てる必要はありません。
- **`$$` は `$` そのものです。** `$${A}` は `${A}` のまま渡ります。
- **変数名でないものは参照ではありません。** `${PORT:-3000}` はシェルの記法
  として、そのままシェルに届きます。
- **どこにも定義の無い名前はエラーです。** 空文字にはしません。プロキシが無い
  ときに `MINATO_URL_<SERVICE>` を未設定のままにするのと同じ理由です。その
  ため `${MINATO_URL_API}` を参照していると、プロキシが動いていない間はその
  変数が欠けたまま起動するのではなく、サービスの起動自体が止まります。復旧の
  手順は `minato doctor` が示します。

::: warning この機能より前に書かれた値について
`${...}` と `$$` には、これまで無かった意味が付きました。既にこれらを含む値は
挙動が変わります。`$$` は `$` 1 文字になり、存在しない変数名を指す `${NAME}`
はそのまま渡されるのではなく `minato up` を止めます。文字として渡したい場合は
ドルを重ねてください（`$` は `$$`、`${` は `$${`）。
:::

::: warning シークレットを他の値に埋め込むことはできません
`PASSWORD` が `op://` や `keychain://` の参照である場合、
`DATABASE_URL = "postgres://user:${PASSWORD}@db/app"` は拒否されます。これらは
コンテナ起動時にメモリ上で解決される値であり、ここで展開すると `minato env ls`
や、そこから書き出されるあらゆる出力に平文が載ってしまいます。

組み立て済みの値をシークレットとして保存するか、2 つの変数のままアプリケー
ションに渡して、そちらで結合してください。
:::

## ファイルに書き出す

起動時の環境変数を読まない道具があります。`wrangler dev` は自身の環境変数を
Worker に渡さず、Vite や dotenvx はディスク上のファイルを読みます。`env_file`
は解決済みの値を、それらが見つけられる場所に書き出します。

```toml
[services.api]
env_file = ".minato/env.api"
```

```sh
wrangler dev --env-file .env --env-file .minato/env.api
```

パスは worktree からの相対で、サービスの起動直前——`minato up` のときと、
scale-to-zero が起こすたび——に書かれます。停止後も残るため、worktree で
`pnpm dev` を直接動かす場合も同じ値を読めます。

**内容が変わらない場合は書き込みません。** ファイルを監視している dev server
が、サービスが起きるたびに再起動してしまうためです。

- **git が追跡しているパスは拒否します。** 生成ファイルは worktree を永久に
  dirty にし、コミットすれば 1 つのブランチの URL が他のすべてのチェックアウト
  に混入します。gitignore された場所——`.minato/` は既にそうです——を指定して
  ください。
- **Minato が書いたのでないファイルは上書きしません。** 目印は先頭行のヘッダ
  です。自分で用意した `.env.local` は安全で、置き換えではなくファイル名を
  含むエラーが返ります。
- **`.minato/env` と `.minato/env.local` は指定できません。** この 2 つは
  Minato 自身が層として読むファイルです。書き出すと生成ファイルがそのまま
  入力に戻り、しかも workspace 層は最も優先度が高いため、前回の値が今回
  注入される値を上書きしてしまいます。隣に別名で書いてください。
- **1 つのパスにつき 1 サービスです。** 2 つのサービスが同じファイルを指すと、
  起動のたびに互いの環境変数を上書きし合います。
- **`scope = "project"` では使えません。** 共有サービスには worktree が
  マウントされないため、そのコンテナから見えない場所に書かれてしまいます。

::: warning シークレットは書き出されません
値が `op://` や `keychain://` の参照であるキーは、コメントに名前だけ残し、
書き出しません。解決済みのシークレットはデーモンのメモリ上にのみ存在し、
ディスクには触れません。ファイルは読み手に渡っていくものなので、ここで書けば
その保証は終わります。

道具がシークレットそのものを必要とする場合は、自前の `.env` を用意して両方の
ファイルを渡してください。
:::

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
