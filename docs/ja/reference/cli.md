# CLI コマンド

すべてのコマンドが `--json` と `-w, --workspace` を受け付けます。

| フラグ | 説明 |
| --- | --- |
| `--json` | 応答を JSON で出力します。エラーも stdout に出力されるため、エージェントは 1 つのストリームのみを監視すれば済みます |
| `-w, --workspace <name>` | 対象の workspace。省略した場合はカレントディレクトリから判定します |

## 初期設定

### `minato init`

リポジトリルートに `minato.toml` のひな形を生成し、ディレクトリ名から
プロジェクト名を推測します。worktree 内で実行した場合も、main worktree に
生成します。

```console
$ minato init
$ minato init --force    # 既存のファイルを上書きする
```

### `minato doctor`

環境を診断し、`✓` 以外のすべての項目に対処方法を表示します。診断対象は、
プロジェクトが使用するランタイム、プロキシと DNS の待ち受け状態、launchd
socket activation、ローカル CA とその信頼状態、`/etc/resolver` の設定、
および名前が実際に 127.0.0.1 へ解決されるかどうかです。

### `minato setup`

管理者権限が必要な設定のコマンドを表示します。対象は LaunchDaemon の配置、
resolver の設定、CA の信頼登録です。**実行は行いません。** 手順は設定「後」の
状態に合わせて生成されます。launchd を配置すると DNS は 53 番ポートに移るため、
resolver に記述するポート番号もそれに合わせたものになります。

## workspace の操作

### `minato new <branch>`

worktree を作成し、環境を起動して URL を表示します。

```console
$ minato new feature/user-auth
$ minato new hotfix/x --base v1.2.0
$ minato new feature/x --path ../elsewhere
$ minato new feature/x --no-start
```

| フラグ | 説明 |
| --- | --- |
| `--base <ref>` | 新規ブランチの分岐元 |
| `--path <dir>` | worktree の作成先。既定値は `../{repo}.wt/{branch}` |
| `--no-start` | 作成のみ行い、起動しない |

既存のブランチは、新規作成せずチェックアウトします。

### `minato ls`

すべての workspace と、稼働中のサービス数を表示します。

```console
$ minato ls
$ minato ls --all-projects   # 現時点ではカレントプロジェクトのみ
```

### `minato status`

対象 workspace の詳細を表示します。各サービスの状態、URL、プロキシの転送先
アドレスが含まれます。

### `minato rm`

worktree とコンテナを削除します。ブランチは残ります。共有サービス
（`scope = "project"`）も、他の worktree が使用しているため残ります。

```console
$ minato rm -w feature-auth
$ minato rm -w feature-auth -f   # 未コミットの変更があっても削除する
```

## サービスの操作

### `minato up [services…]`

サービスとその依存先を起動します。サービス名を省略した場合はすべてが対象です。

稼働中のコンテナには変更を加えません。停止中のコンテナは、設定変更を反映する
ため再作成します。

### `minato down [services…]`

```console
$ minato down
$ minato down web
$ minato down --all    # プロジェクト内の全 workspace
```

共有サービスは、名前を明示的に指定した場合のみ停止します。他の worktree が
使用している可能性があるためです。

### `minato url [service]`

1 行のみを出力します。サービス名を省略した場合は、最初にアクセス可能な
サービスが対象です。

```console
$ curl -sS --fail-with-body "$(minato url web)/api/health"
```

停止中でも URL は有効です。リクエストによって起動します。

### `minato logs [services…]`

```console
$ minato logs
$ minato logs web -n 100
$ minato logs web -f
```

| フラグ | 説明 |
| --- | --- |
| `-f, --follow` | 継続的に出力する |
| `-n, --tail <n>` | 末尾から表示する行数 |

装飾を含まず、stdout と stderr は分離されたままです。

### `minato exec <service> -- <command>`

```console
$ minato exec web -- npm test
$ minato exec web -- sh
```

**終了コードは実行したコマンドのものです。** TTY は要求しないため、入力待ちに
なるコマンドはプロンプトを表示せず停止します。

## 環境変数

```console
$ minato env ls [--reveal]
$ minato env get <KEY>
$ minato env set <KEY=VALUE> [--scope global|project|workspace]
$ minato env unset <KEY> [--scope …]
```

`ls` は定義元の層を表示し、シークレットはマスクします。`--reveal` を指定すると
平文の値が表示されますが、シークレット「参照」は参照のまま表示されます。
`get` はパイプで利用できるよう、値を 1 行だけ出力します。

`--scope` の既定値は `workspace` です。

## トンネル

```console
$ minato tunnel enable --domain example.com --public
$ minato tunnel disable
$ minato tunnel status
```

`--public` は必須です。Minato が検証できない状態でインターネットに公開する
ことへの同意を意味します。ドメインは初回実行時に保存されます。

## エージェント

```console
$ minato skill install [--force]
$ minato skill show
```

`.claude/skills/minato/SKILL.md` を生成します。内容に変更がなければ書き込みを
行いません。

## daemon

```console
$ minato daemon start
$ minato daemon stop
$ minato daemon status
```

いずれのコマンドも daemon が停止していれば自動的に起動するため、これらの操作
はほとんど必要ありません。LaunchDaemon を配置したマシンでは、`stop` の直後に
launchd が再起動します。80/443 番ポートを保持したまま設定を再読み込みする
手段です。

## Minato 自体の設定に使う環境変数

| 変数 | 説明 |
| --- | --- |
| `MINATO_HOME` | 状態、ログ、ソケット、CA の保存先。既定値 `~/.minato` |
| `MINATO_HTTP_PORT` | プロキシの HTTP ポート。既定値 80 |
| `MINATO_HTTPS_PORT` | プロキシの HTTPS ポート。既定値 443 |
| `MINATO_DNS_PORT` | DNS のポート。既定値 53 |
| `MINATO_CLOUDFLARED` | `PATH` 以外に配置された `cloudflared` のパス |
| `MINATO_LOG` | daemon のログフィルタ。例: `debug` |
