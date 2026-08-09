# CLI コマンド

すべてのコマンドが `--json` と `-w, --workspace` を受け付けます。

| フラグ | 説明 |
| --- | --- |
| `--json` | 応答を JSON で出力します。エラーも stdout に出力されるため、エージェントは 1 つのストリームのみを監視すれば済みます |
| `-w, --workspace <name>` | 対象の workspace。省略した場合はカレントディレクトリから判定します |

## 出力の見え方

端末に向けて出力する場合、結果は「描画」されます。枠で囲まれたパネル、桁の
揃った表、そして意味を持つ部分——サービスの状態、URL、実行を促されている
コマンド——への色付けです。時間のかかるコマンドは最下行を「いま起きている
こと」のために確保し、終わったステップはその上へ流していきます。

パイプ・リダイレクト・キャプチャの先では、同じ内容が素のテキストになります。
枠も色もカーソル移動もなく、URL がどれだけ長くても折り返しも切り詰めも
起きません。`minato status | grep web` はこれまでどおりに読めます。

| | |
| --- | --- |
| `--json` | 出力先にかかわらず、常に装飾しません |
| `NO_COLOR` | 何かしら設定されていれば、レイアウトはそのままに色だけを落とします |
| `TERM=dumb` | 全面的にパイプと同じ扱いにします |
| `minato url` / `minato env get` | 常に 1 行だけ。他のコマンドへ埋め込むためのものです |
| `minato logs` / `minato exec` | そのまま素通しし、stdout と stderr を分けたまま渡します |

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

管理者権限が必要な設定を、1 手順ずつ確認しながら進めます。対象は LaunchDaemon
の配置、resolver の設定、CA の信頼登録です。各手順は実行するコマンドを表示した
うえで実行するかどうかを尋ね、**同意した手順だけを実行します。** 応答できる端末
がない場合——エージェント、パイプ、`--json`——はコマンドを表示するだけで、
何も実行しません。

```console
$ minato setup
$ minato setup --yes       # 確認せずすべて実行する
$ minato setup --dry-run   # コマンドを表示するだけで実行しない
```

| フラグ | 説明 |
| --- | --- |
| `-y`, `--yes` | 確認せずにすべての手順を実行する |
| `--dry-run` | コマンドを表示し、何も実行しない |

手順は設定「後」の状態に合わせて生成されます。launchd を配置すると DNS は 53
番ポートに移るため、resolver に記述するポート番号もそれに合わせたものになります。
launchd の手順を実行しなかった場合、resolver の手順は現在 DNS が使用している
ポートに合わせて書き換えられます。ある手順を断ったことで次の手順が壊れることは
ありません。

実行しなかった手順と、コマンドが失敗した手順は、最後にまとめて表示されます。
失敗した手順があれば終了コードは 0 以外になります。実行しなかっただけの手順は
失敗ではありません。

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
| `--build` | 変更がなくてもイメージを再ビルドする |

既存のブランチは、新規作成せずチェックアウトします。

### `minato ls`

すべての workspace と、稼働中のサービス数を表示します。

```console
$ minato ls
$ minato ls --all-projects   # この daemon が把握している全プロジェクト
```

`--all-projects` を指定すると `PROJECT` 列が追加されます。他プロジェクトに
ついては**登録済みの** worktree のみが対象です。未登録のものを探すには他の
リポジトリを開く必要があるためで、そのプロジェクト内で一度もコマンドを実行
していない場合は、内側から見たときより行数が少なくなります。

### `minato status`

対象 workspace の詳細を表示します。各サービスの状態、URL、プロキシの転送先
アドレスが含まれます。

| 状態 | 意味 |
| --- | --- |
| `stopped` | コンテナが無いか、停止済み。URL にアクセスすると起動します |
| `starting` | コンテナは起動しているが、`health` チェックがまだ応答していない |
| `ready` | 応答している |
| `failed` | 異常終了した。`reason` に理由が入ります |

::: tip `ready` の検証は `health` が HTTP チェックのときだけ行われます
コンテナが起動していることと、中のアプリが応答できることは別です。
`health = "http://..."` を指定していれば、ready と報告する前にそのチェックを
実行します。ビルド中の dev server は `starting` になり、待つべきかどうかを
判断できます。

指定がない場合、`ready` は「コンテナが起動している」という意味になります。
外から分かるのはそこまでです。接続可否を見ても意味はありません。Docker は
ポートを公開する際に前段へフォワーダを置き、**コンテナ内で何も listen して
いなくてもフォワーダは接続を受け付ける**ためです。`ready` に「応答している」
という意味を持たせたい場合は [`health`](./minato-toml#起動完了の判定) を
設定してください。
:::

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

| フラグ | 説明 |
| --- | --- |
| `--build` | Minato が検知できる変更がなくてもイメージを再ビルドする |

稼働中のコンテナには、イメージが変わっていない限り変更を加えません。停止中の
コンテナは、設定変更を反映するため再作成します。

`--build` は fingerprint では検知できない変更、たとえば Dockerfile が COPY
するファイルの変更に対応するためのものです。

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

## 中断する

Ctrl-C は CLI をその場で終了させるのではなく、daemon に停止を依頼して応答を
待ちます。終了コードは 130 です。

すでに完了した処理は取り消されません。中断された `up` はコンテナを起動した
ままにする場合があり、その状態は `minato status` で確認でき、`minato down` で
片付けられます。

`minato logs -f` は例外です。Ctrl-C はこれを終了させる通常の手段です。

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

## 更新

```console
$ minato update
$ minato update --check
```

実行した `minato` が置かれているディレクトリの 2 つのバイナリを、現在の
`nightly` に差し替えます。`--check` は結果を表示するだけで何もインストール
しません。`--json` の出力は次の形です。

```json
{ "status": "available", "commit": "…", "running": "…" }
```

`status` は `current` / `available` / `installed` / `unknown` のいずれかです。
`unknown` は、そのビルドがコミットを記録しておらず比較できないことを表します。

チェックはコマンドの実行後に 1 日 1 回自動で走り、stderr に 1 行表示します。
`MINATO_NO_UPDATE_CHECK` で停止でき、`--json` のときは表示しません。

## アンインストール

```console
$ minato uninstall
╭ uninstall ─────────────────────────────────────────────────────────────────╮
│ containers:                                                                │
│ myapp / main               web                                             │
│ myapp / main               db                                              │
│ myapp / feature-user-auth  web                                             │
│                                                                            │
│ files:                                                                     │
│ state, logs and the local CA  /home/u/.minato                              │
│ shell completions             /home/u/.config/fish/completions/minato.fish │
│ the binary                    /home/u/.local/bin/minato                    │
│ the binary                    /home/u/.local/bin/minatod                   │
│                                                                            │
│ needs root:                                                                │
│   stop the LaunchDaemon holding 80/443/53                                  │
│     sudo launchctl bootout system/dev.minato.daemon                        │
│     sudo rm /Library/LaunchDaemons/dev.minato.daemon.plist                 │
│   stop trusting the local CA                                               │
│     sudo security remove-trusted-cert -d ~/.minato/ca/minato-ca.crt        │
│                                                                            │
│ left alone — 2 worktrees:                                                  │
│   /path/to/myapp                                                           │
│   /path/to/myapp.wt/feature-user-auth                                      │
╰────────────────────────────────────────────────────────────────────────────╯
Remove all of this? [y/N]
```

| フラグ | 説明 |
| --- | --- |
| `-y, --yes` | 確認せずに実行します。端末がない場合は必須です |
| `--dry-run` | 一覧を表示するだけで、何も削除しません |

**worktree には一切触れません。** あなたのチェックアウトであり、コミットして
いない変更が入っているためです。削除は `minato rm` が 1 つずつ行い、git が
拒否する場合は `--force` を求めます。何が残るかが分かるよう、一覧には表示
します。

存在しないものは一覧に出しません。つまりこの一覧は「Minato が置いた可能性の
ある場所」ではなく、実際にこのマシンにあるものです。`cargo build` の出力に
ついてはバイナリを削除しません。チェックアウトから `uninstall` を実行しても、
消えるのはインストール済みのものだけで、ビルド成果物は残ります。

root が必要な手順は `sudo` で実行し、パスワードを尋ねます。入力できる端末が
ない場合——エージェント、パイプ、CI——は `minato setup` と同じくコマンドを
表示するにとどめ、それ以外の削除は続行します。

## 補完

```console
$ minato completions <bash|zsh|fish|elvish|powershell>
```

スクリプトを標準出力に書き出します。各シェルの配置先は
[インストール](../guide/installation#シェル補完) を参照してください。
インストールスクリプトを使った場合は設定済みです。

## Minato 自体の設定に使う環境変数

| 変数 | 説明 |
| --- | --- |
| `MINATO_HOME` | 状態、ログ、ソケット、CA の保存先。既定値 `~/.minato` |
| `MINATO_HTTP_PORT` | プロキシの HTTP ポート。既定値 80、確保できない場合は 18080。明示した場合はその値がそのまま使われます |
| `MINATO_HTTPS_PORT` | プロキシの HTTPS ポート。既定値 443、確保できない場合は 18443。明示した場合はその値がそのまま使われます |
| `MINATO_DNS_PORT` | DNS のポート。既定値 53 |
| `MINATO_CLOUDFLARED` | `PATH` 以外に配置された `cloudflared` のパス |
| `MINATO_LOG` | daemon のログフィルタ。例: `debug` |
| `MINATO_NO_UPDATE_CHECK` | 何か値を設定すると 1 日 1 回の更新チェックを行いません |
