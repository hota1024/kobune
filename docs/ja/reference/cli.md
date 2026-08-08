# CLI コマンド

すべてのコマンドが `--json` と `-w, --workspace` を受け付けます。

| フラグ | |
| --- | --- |
| `--json` | 応答を JSON で出力。エラーも stdout に出るので、エージェントは 1 本のストリームだけを見れば済みます |
| `-w, --workspace <name>` | 対象の workspace。省略時は現在のディレクトリから判定 |

## 準備

### `minato init`

リポジトリルートに `minato.toml` のひな形を書き、ディレクトリ名から
プロジェクト名を推測します。worktree の中で実行しても main worktree に
書きます。

```console
$ minato init
$ minato init --force    # 既存を上書き
```

### `minato doctor`

環境を診断し、`✓` でないものすべてに直し方を出します。プロジェクトが使う
ランタイム、プロキシと DNS の待ち受け、launchd socket activation、ローカル CA
とその信頼、`/etc/resolver`、そして実際に 127.0.0.1 に解決されるかを見ます。

### `minato setup`

root が要る部分のコマンドを表示します。LaunchDaemon、resolver の設定、CA の
信頼登録。**実行はしません。** 手順は設定 *後* の状態に合わせて生成されます
—— launchd を設置すると DNS は :53 に移るので、resolver に書くポートもそう
なります。

## workspace

### `minato new <branch>`

worktree を作り、環境を起動し、URL を出します。

```console
$ minato new feature/user-auth
$ minato new hotfix/x --base v1.2.0
$ minato new feature/x --path ../elsewhere
$ minato new feature/x --no-start
```

| フラグ | |
| --- | --- |
| `--base <ref>` | 新規ブランチの分岐元 |
| `--path <dir>` | worktree の置き場所。既定は `../{repo}.wt/{branch}` |
| `--no-start` | 作るだけで起動しない |

既存のブランチは作らずにチェックアウトします。

### `minato ls`

すべての workspace と、いくつのサービスが動いているか。

```console
$ minato ls
$ minato ls --all-projects   # 現状はまだ現在のプロジェクトのみ
```

### `minato status`

現在の workspace の詳細。各サービスの状態、URL、プロキシの転送先アドレス。

### `minato rm`

worktree とコンテナを削除します。ブランチは残り、共有サービス
（`scope = "project"`）も他の worktree が使うので残ります。

```console
$ minato rm -w feature-auth
$ minato rm -w feature-auth -f   # 未コミットの変更があっても
```

## サービス

### `minato up [services…]`

サービスと、その依存先を起動します。名前がなければすべて。

動いているコンテナには触れません。停止中のものは設定変更を反映するため
作り直します。

### `minato down [services…]`

```console
$ minato down
$ minato down web
$ minato down --all    # プロジェクト内のすべての workspace
```

共有サービスは、名前を明示したときだけ止まります。他の worktree が使って
いるかもしれないからです。

### `minato url [service]`

1 行だけを出力します。名前がなければ最初の到達可能なサービス。

```console
$ curl -sS --fail-with-body "$(minato url web)/api/health"
```

停止中でも URL は有効です。リクエストが起動させます。

### `minato logs [services…]`

```console
$ minato logs
$ minato logs web -n 100
$ minato logs web -f
```

| フラグ | |
| --- | --- |
| `-f, --follow` | 流し続ける |
| `-n, --tail <n>` | 末尾から何行 |

装飾なしで、stdout と stderr は分かれたままです。

### `minato exec <service> -- <command>`

```console
$ minato exec web -- npm test
$ minato exec web -- sh
```

**終了コードはコマンドのものです。** TTY は要求しないので、入力待ちの
コマンドはプロンプトを出さずに固まります。

## 環境変数

```console
$ minato env ls [--reveal]
$ minato env get <KEY>
$ minato env set <KEY=VALUE> [--scope global|project|workspace]
$ minato env unset <KEY> [--scope …]
```

`ls` はどの層の値かを表示し、シークレットは伏せます。`--reveal` で平文の値は
出ますが、シークレット *参照* は参照のままです。`get` はパイプ用に 1 つの値を
出します。

`--scope` の既定は `workspace` です。

## トンネル

```console
$ minato tunnel enable --domain example.com --public
$ minato tunnel disable
$ minato tunnel status
```

`--public` は必須で、Minato が確認できない状態でインターネットに出すことを
承認します。ドメインは初回以降記憶されます。

## エージェント

```console
$ minato skill install [--force]
$ minato skill show
```

`.claude/skills/minato/SKILL.md` を書き出します。内容が同じなら書き直しません。

## daemon

```console
$ minato daemon start
$ minato daemon stop
$ minato daemon status
```

どのコマンドも止まっていれば daemon を起動するので、ほとんど必要ありません。
LaunchDaemon を設置したマシンでは `stop` の直後に launchd が起動し直します
—— 80/443 を保ったまま新しい設定を読む手段です。

## Minato 自身を設定する環境変数

| | |
| --- | --- |
| `MINATO_HOME` | 状態・ログ・ソケット・CA の置き場所。既定 `~/.minato` |
| `MINATO_HTTP_PORT` | プロキシの HTTP ポート。既定 80 |
| `MINATO_HTTPS_PORT` | プロキシの HTTPS ポート。既定 443 |
| `MINATO_DNS_PORT` | DNS のポート。既定 53 |
| `MINATO_CLOUDFLARED` | `PATH` 以外にある `cloudflared` |
| `MINATO_LOG` | daemon のログフィルタ。例 `debug` |
