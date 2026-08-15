# CLI コマンド

すべてのコマンドが `--json` と `-w, --workspace` を受け付けます。

| フラグ | 説明 |
| --- | --- |
| `--json` | 応答を JSON で出力します。エラーも stdout に出力されるため、エージェントは 1 つのストリームのみを監視すれば済みます |
| `-w, --workspace <name>` | 対象の workspace。省略した場合はカレントディレクトリから判定します |

## 出力の見え方

端末に向けて出力する場合、結果は「描画」されます。枠で囲まれたパネル、桁の
揃った表、そして意味を持つ部分への色付けです。色が付くのはサービスの状態・
URL・実行を促されているコマンドです。時間のかかるコマンドは最下行を「いま
起きていること」のために確保し、終わったステップはその上へ流していきます。

パイプ・リダイレクト・キャプチャの先では、同じ内容が素のテキストになります。
枠も色もカーソル移動もなく、URL がどれだけ長くても折り返しも切り詰めも
起きません。`kobune status | grep web` はこれまでどおりに読めます。

| | |
| --- | --- |
| `--json` | 出力先にかかわらず、常に装飾しません |
| `NO_COLOR` | 何かしら設定されていれば、レイアウトはそのままに色だけを落とします |
| `TERM=dumb` | 全面的にパイプと同じ扱いにします |
| `kobune url <service>` / `kobune env get` | 常に 1 行だけ。他のコマンドへ埋め込むためのものです |
| `kobune logs` / `kobune exec` | そのまま素通しし、stdout と stderr を分けたまま渡します |
| `kobune logs -f <service>` | `tty` があれば端末はサービスのものになります。[サービスに入力する](#サービスに入力する)を参照 |

## 初期設定

### `kobune init`

リポジトリルートに `kobune.toml` のひな形を生成し、ディレクトリ名から
プロジェクト名を推測します。worktree 内で実行した場合も、main worktree に
生成します。

```console
$ kobune init
$ kobune init --force    # 既存のファイルを上書きする
```

#### compose ファイルから変換する

```console
$ kobune init --from-compose              # compose.yaml、docker-compose.yml などを探す
$ kobune init --from-compose infra.yml    # ファイルを指定する
```

**意図的に完全な変換ではありません。** compose は巨大で、その半分は Kobune では
意味を持ちません。そのためすべてのキーは 3 つのいずれかに振り分けられ、黙って
消えるものはありません。

- **変換される** — `image`、`build`、`ports`、`expose`、`command`、
  `environment`、`depends_on`、`volumes`、`healthcheck`、`working_dir`、`tty`
- **`TODO` としてサービスの直上に残る** — compose では表現できないもの。
  データベースを worktree 間で共有するか、`setup` に何を実行させるか
- **レポートに列挙される** — `restart`、`deploy`、`networks`、`logging` など。
  サービスごとに

最初の `kobune up` の前に TODO を読んでください。完成しているように見えて
そうではないファイルは、変換しないことより高くつきます。

特に知っておく価値のある変換が 2 つあります。

- **`env_file` は `carry` になります。** このキーは 2 つの形式で意味が正反対
  です。compose はファイルを*読み*、Kobune は書き出します。そのまま対応付ける
  と、最初の `up` であなたの `.env` を上書きしてしまいます。`carry` が実際に
  意味するもの、つまり新しい worktree に必要で git が持ってこないファイル、に
  相当します
- **`ports: ["3000:8000"]` はコンテナ側の `8000` を採ります。** Kobune は自身が
  選んだポートで公開するため、必要なのはアプリがコンテナ内で待ち受けている
  ポートです

### `kobune doctor`

環境を診断し、`✓` 以外のすべての項目に対処方法を表示します。診断対象は、
プロジェクトが使用するランタイム、プロキシと DNS の待ち受け状態、launchd
socket activation、ローカル CA とその信頼状態、`/etc/resolver` の設定、
および名前が実際に 127.0.0.1 へ解決されるかどうかです。

### `kobune setup`

管理者権限が必要な設定を、1 手順ずつ確認しながら進めます。対象は LaunchDaemon
の配置、resolver の設定、CA の信頼登録です。各手順は実行するコマンドを表示した
うえで実行するかどうかを尋ね、**同意した手順だけを実行します。** 応答できる端末
がないとき、つまりエージェント・パイプ・`--json` では、コマンドを表示する
だけで何も実行しません。

```console
$ kobune setup
$ kobune setup --yes       # 確認せずすべて実行する
$ kobune setup --dry-run   # コマンドを表示するだけで実行しない
```

| フラグ | 説明 |
| --- | --- |
| `-y`, `--yes` | 確認せずにすべての手順を実行する |
| `--dry-run` | コマンドを表示し、何も実行しない |

手順は設定「後」の状態に合わせて生成されます。launchd を配置すると DNS は 53
番ポートに移るため、resolver に書くポート番号もそれに合わせたものになります。
launchd の手順を実行しなかった場合、resolver の手順は現在 DNS が使用している
ポートに合わせて書き換えられます。ある手順を断ったことで次の手順が壊れることは
ありません。

**launchd に登録済みの LaunchDaemon は再インストールされません。** その場合の
手順はジョブの起動に変わります。登録済みのラベルに対する 2 度目の `bootstrap`
は `Input/output error` として拒否されるため、再インストールは失敗するほかない
からです。plist が配置されているだけで `bootstrap` されていない場合は未設定と
みなし、インストールの手順を提示します。

実行しなかった手順と、コマンドが失敗した手順は、最後にまとめて表示されます。
失敗した手順があれば終了コードは 0 以外になります。実行しなかっただけの手順は
失敗ではありません。

## workspace の操作

### `kobune new <branch>`

worktree を作成し、環境を起動して URL を表示します。

```console
$ kobune new feature/user-auth
$ kobune new hotfix/x --base v1.2.0
$ kobune new feature/x --path ../elsewhere
$ kobune new feature/x --no-start
```

| フラグ | 説明 |
| --- | --- |
| `--base <ref>` | 新規ブランチの分岐元 |
| `--path <dir>` | worktree の作成先。既定値は `../{repo}.wt/{branch}` |
| `--no-start` | 作成のみ行い、起動しない |
| `--build` | 変更がなくてもイメージを再ビルドする |

既存のブランチは、新規作成せずチェックアウトします。

### `kobune ls`

すべての workspace と、稼働中のサービス数を表示します。

```console
$ kobune ls
$ kobune ls --all-projects   # この daemon が把握している全プロジェクト
```

`--all-projects` を指定すると `PROJECT` 列が追加されます。他プロジェクトに
ついては**登録済みの** worktree のみが対象です。未登録のものを探すには他の
リポジトリを開く必要があるためで、そのプロジェクト内で一度もコマンドを実行
していない場合は、内側から見たときより行数が少なくなります。

### `kobune status`

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
という意味を持たせたい場合は [`health`](./kobune-toml#起動完了の判定) を
設定してください。
:::

### `kobune rm`

worktree とコンテナを削除します。ブランチは残ります。共有サービス
（`scope = "project"`）も、他の worktree が使用しているため残ります。

```console
$ kobune rm -w feature-auth
$ kobune rm -w feature-auth -f   # 未コミットの変更があっても削除する
```

## サービスの操作

### `kobune up [services…]`

サービスとその依存先を起動します。サービス名を省略した場合はすべてが対象です。

| フラグ | 説明 |
| --- | --- |
| `--build` | Kobune が検知できる変更がなくてもイメージを再ビルドする |

稼働中のコンテナには、イメージが変わっていない限り変更を加えません。停止中の
コンテナは、設定変更を反映するため再作成します。

`--build` は fingerprint では検知できない変更、たとえば Dockerfile が COPY
するファイルの変更に対応するためのものです。

### `kobune down [services…]`

```console
$ kobune down
$ kobune down web
$ kobune down --all    # プロジェクト内の全 workspace
```

共有サービスは、名前を明示的に指定した場合のみ停止します。他の worktree が
使用している可能性があるためです。

### `kobune url [service] [--qr]`

サービス名を指定した場合は 1 行のみを出力します。他のコマンドへ埋め込むため
のものです。

```console
$ curl -sS --fail-with-body "$(kobune url web)/api/health"
```

サービス名を省略した場合は、全サービスとアクセス先を一覧表示します。外から
入れないサービスや、トンネルが有効なときはトンネル URL も含みます。

```console
$ kobune url
web   https://web.feat-1.myapp.localhost
api   https://api.feat-1.myapp.localhost
db    internal only
```

`--qr` は URL を QR コードとして描画します。スマートフォンで開くためのもの
です。トンネル URL があればそちらを使います。`.localhost` の名前はこのマシン
でしか解決できないため、それしかない場合はその旨を添えます。

```console
$ kobune url web --qr
```

停止中でも URL は有効です。リクエストによって起動します。

### `kobune logs [services…]`

```console
$ kobune logs
$ kobune logs web -n 100
$ kobune logs web -f
$ kobune logs -f dev          # `tty` を持つサービス: そのまま入力できます
```

| フラグ | 説明 |
| --- | --- |
| `-f, --follow` | 継続的に出力する |
| `-n, --tail <n>` | 末尾から表示する行数 |
| `--no-input` | 読み取り専用。端末を渡しません |

装飾を含まず、stdout と stderr は分離されたままです。

#### サービスに入力する

[`tty`](./kobune-toml#tty) を設定したサービスは端末を持ちます。
`kobune logs -f` はそこに手元の端末を貸し出します。色がそのまま通り、全画面
インターフェイスが描画され、キー入力がプログラムに届きます。Turborepo のタスク
切り替えや、監視モードのテストランナーが Kobune 上で動くのはこの仕組みです。

「そう解釈するほかない」場合にだけ自動で有効になります。すなわち、端末上で、
`--json` なしに、**サービスを 1 つだけ指定して** `-f` で追いかけたときです。
パイプ、エージェント、サービス名を省いた `kobune logs -f` は、従来どおりの
素のストリームのままです。`--no-input` は、誤って入力してしまわないように
見るだけにしたいときに使います。

| キー | 動作 |
| --- | --- |
| Ctrl-P Ctrl-Q | デタッチします。サービスは動いたままです |
| それ以外 | Ctrl-C を含め、すべてプログラムに渡されます |
| マウス・トラックパッド | プログラムが求めていれば、そちらに渡されます |

**ホイールはプログラムがスクロールするものをスクロールします。** 全画面
プログラムがマウス報告を要求するのは、最初に書き出す数バイトのなかで一度きり
です。1 時間後にアタッチした端末はその要求を聞き逃すので、本来なら端末は何も
送らず、ホイールは何も起こしません。そこで daemon はコンテナが動き出す前から
サービスの端末を聞いており、プログラムが端末に対して行った設定を覚えていて、
アタッチ時にここで設定し直します。Turborepo のログペインは手元で直接実行した
ときと同じようにポインタの下でスクロールします。デタッチすると元に戻します。

**daemon を再起動すると失われます。** その要求を聞いたのは一度きりで、聞いた
daemon はもういません。読み直せる場所もありません。ログはその場所になりません。
Docker はプログラムの最後の改行より後ろを、そのプログラムが終わるまで抱えて
います。全画面プログラムは行を終えないので、そのバイトがログに現れるのは
サービスが終わったあと、つまり使い道がなくなってからです。サービスは動き続け、
キー入力も届きますが、マウスと代替画面は `kobune down && kobune up` でコンテナ
を起動し直すまで戻りません。

Ctrl-C はプログラムのものです。タスクランナーではたいてい「終了」を意味し、
結果としてサービスが止まります。何も止めずに抜けるには Ctrl-P Ctrl-Q を使って
ください（`docker attach` と同じ並びです）。

端末を持たないサービスを指定してもエラーにはなりません。Kobune はその旨を
1 行で伝え、通常どおりログを流します。

::: warning Apple Container は起動時にサイズを固定します
端末のサイズはサービスの起動時に決まり、その後は変更できません。そのため全画面
プログラムはウィンドウの大きさにかかわらず 120×40 に描画します。アタッチ時に
その旨を表示します。Docker はウィンドウに追従します。
:::

### `kobune exec <service> -- <command>`

```console
$ kobune exec web -- npm test
$ kobune exec web -- sh
$ kobune exec -C /workspace/apps/api api -- pnpm test   # 別のディレクトリで
```

**終了コードは実行したコマンドのものです。** TTY は要求しないため、入力待ちに
なるコマンドはプロンプトを表示せず停止します。

`-C` は作業ディレクトリを指定します。省略時はサービスの
[`workdir`](./kobune-toml#イメージとコマンド) です。`-w` ではなく `-C` なのは、
`-w` が workspace の指定に使われているためです。

#### `--fresh`

```console
$ kobune exec --fresh api -- env
$ kobune exec --fresh api -- cat /workspace/.env
$ kobune exec --fresh api -- sh -c 'pnpm install --frozen-lockfile'
```

標準入力は接続しないため、`-- sh` だけを渡すと即座に EOF を読んで終了します。
実行したい内容は `sh -c` に渡してください。

そのコマンドのためだけのコンテナを立てて実行し、終了後に削除します。
サービスのイメージ・環境変数・ボリュームはそのままに、**サービスの起動
コマンドは実行しません**。

**サービスが起動している必要はありません。** これが要点です。起動スクリプトが
失敗した状態では exec する先が存在せず、しかもそのときこそ中を見たいためです。

ポートは公開せず、Kobune のラベルも付けません。そのため実際のコンテナから
ポートを奪うことも、`kobune status` に現れることも、ネットワーク上でサービス名に
応答することもありません。イメージが未取得の場合は先に取得・ビルドするため、
一度も正常に起動していないサービスに対しても使えます。

## 中断する

Ctrl-C は CLI をその場で終了させるのではなく、daemon に停止を依頼して応答を
待ちます。終了コードは 130 です。

すでに完了した処理は取り消されません。中断された `up` はコンテナを起動した
ままにする場合があり、その状態は `kobune status` で確認でき、`kobune down` で
片付けられます。

`kobune logs -f` は例外です。Ctrl-C はこれを終了させる通常の手段です。
[端末を渡している](#サービスに入力する)場合、Ctrl-C はプログラムのものになり、
抜けるには Ctrl-P Ctrl-Q を使います。

## 環境変数

```console
$ kobune env ls [--reveal] [--service <name>]
$ kobune env get <KEY>
$ kobune env set <KEY=VALUE> [--scope global|project|workspace]
$ kobune env unset <KEY> [--scope …]
```

`ls` は定義元の層を表示し、シークレットはマスクします。`--reveal` を指定すると
平文の値が表示されますが、シークレット「参照」は参照のまま表示されます。
`get` はパイプで利用できるよう、値を 1 行だけ出力します。

`--service` は、そのコンテナに実際に渡される内容を表示します。サービス固有の
[`env`](./kobune-toml#環境変数) も含まれるため、`kobune env ls --service api`
で「`KOBUNE_URL_WEB` は本当に `api` に届いているか」を、何も起動せずに確認
できます。指定しない場合は全サービスに共通する内容だけです。サービス固有の
`env` はそのサービスのものであり、対象となるサービスが無いため
`KOBUNE_SERVICE` も含まれません。`get` にも同じ指定ができます。

解決できない `${...}` がある場合、`ls` は失敗せず、**その値だけ**を書かれた
まま表示して理由を下に添えます。原因の値は値を眺めてしか見つけられず、解決
できた値はそのまま解決済みで表示されます。`--json` では該当の値に `unsettled`
オブジェクト（`reference` と、`undefined`・`only_with_service`・`needs_proxy`・
`secret`・`cycle` のいずれかの `reason`）が付きます。解決できた値にこの
フィールドはありません。一方 `get` は終了コード 7 で失敗します。出力した値が
そのまま使われる前提だからです。

層は内側が優先で 5 つあります。`injected`、`global`、`project`、`service`、
`workspace` です。`service` は `kobune.toml` のサービス固有 `env` を指します。
これを `project` と表示すると、サービス側が上書きしている値のために
`.kobune/env` を編集させてしまうため、独立した名前にしています。

`--scope` の既定値は `workspace` です。

## トンネル

```console
$ kobune tunnel enable --domain example.com --public
$ kobune tunnel disable
$ kobune tunnel status
```

`--public` は必須です。Kobune が検証できない状態でインターネットに公開する
ことへの同意を意味します。ドメインは初回実行時に保存されます。

`--domain` にはゾーン自体を指定します（`dev.example.com` ではなく
`example.com`）。ゾーンの Universal SSL 証明書が覆うのは 1 階層下までで、
トンネルのホスト名はちょうどそこに位置するためです。

## エージェント

```console
$ kobune skill install [--force]
$ kobune skill show
```

`.claude/skills/kobune/SKILL.md` を生成します。内容に変更がなければ書き込みを
行いません。

## daemon

```console
$ kobune daemon start
$ kobune daemon stop
$ kobune daemon restart
$ kobune daemon status
```

いずれのコマンドも daemon が停止していれば自動的に起動するため、これらの操作
はほとんど必要ありません。LaunchDaemon を配置したマシンでは daemon の起動が
launchd 経由になります。これが 80/443 番ポートを保持したままにする仕組みです。
`stop` はジョブを取り外すのではなく待機状態にするもので、ポートは launchd が
保持し続け、次のリクエストで daemon が起動し直します。そのリクエストを待たずに
戻すのが `restart` です。

`restart` があるのは、自動では解消しない唯一のケースのためです。古いビルドの
まま動き続けている daemon がそれにあたります。コマンドには普通に応答しますが、
新しい CLI が話すプロトコルとは食い違い、次のように表示されます。

```
error: the daemon speaks protocol 3, which this kobune (protocol 5) cannot
talk to. Restart it with `kobune daemon restart`
```

バイナリを更新しても、すでに起動しているプロセスは置き換わりません。

`start` と `restart` は、起動したものが launchd のジョブでなかった場合に失敗
します。daemon 自体は動いていても、80・443・53 を保持しているのは起動しな
かったジョブのほうで、どの URL も応答しません。終了コードは 1 になり、どの
状態にあるのかは hint が示します。これで終了コードが変わるのはこの 2 つだけ
です。`kobune up` などは依頼された処理自体は完了しているため、同じ内容を
notice として表示します。

`restart` にはもう 1 つ失敗する場合があります。置き換えるはずだった daemon が
残っているときです。停止には 5 秒の猶予がありますが、それを越えて残った daemon
は起動側がソケットをつかみに行った時点でまだ保持しています。応答自体は普通に
返ってきますが、再起動は起きていません。

```
error: the daemon outlasted every stop, so nothing was restarted: what is
answering has been up 1h 0m
```

停止中に launchd が起こした daemon も、まったく同じ応答を返します。そちらは
restart が望んだとおりの状態です。両者を見分けるのは稼働時間です。起こされた
ほうは restart が待っている間に始まっており、居座ったほうは停止前の稼働時間に
その待ち時間が上乗せされています。

## 更新

```console
$ kobune update
$ kobune update --check
```

実行した `kobune` が置かれているディレクトリの 2 つのバイナリを、現在の
`nightly` に差し替えます。`--check` は結果を表示するだけで何もインストール
しません。`--json` の出力は次の形です。

```json
{ "status": "available", "commit": "…", "running": "…" }
```

`status` は `current` / `available` / `installed` / `unknown` のいずれかです。
`unknown` は、そのビルドがコミットを記録しておらず比較できないことを表します。

`installed` のときは `next` も返します。バイナリが入れ替わったいま実行する価値
のあるコマンドと、そう言える根拠になっている状態です。

```json
{
  "status": "installed",
  "commit": "…",
  "next": [
    { "command": "kobune daemon restart", "reason": "the daemon is still the previous build" }
  ]
}
```

やることがなければ空です（daemon が動いていなかった場合など）。ここに入るのは、
置き換えられる側のビルドが確実に言えることだけです。残りは入った側のビルドが
最初の実行時に表示します。`kobune changed to 9f3c1a2 since the last run` の下に
1 行ずつ、**stderr** へ、ビルドごとに 1 回、`--json` では表示しません。Skill の
行が見るのは実行したリポジトリです。最後に動いたコミットは
`~/.kobune/build.json` に、上の行を表示したあとで書きます。`kobune daemon` には
通知を付けません。`stop` はソケットが閉じる前に戻るため、いま退場を頼んだ
daemon について答えることになるからです。

チェックはコマンドの実行後に 1 日 1 回自動で走り、stderr に 1 行表示します。
`KOBUNE_NO_UPDATE_CHECK` で停止でき、`--json` のときは表示しません。

`kobune --version` でもチェックします。こちらは 1 日 1 回ではなく毎回、
そしてバージョンの行より前ではなく、あとに表示します。

```console
$ kobune --version
kobune 0.1.0 (c7282b8)
› a newer build is available (9f3c1a2). Install it with kobune update
```

この行も stderr です。公開されているビルドと同じなら何も表示しません。
`--json` と `KOBUNE_NO_UPDATE_CHECK` は 1 日 1 回のチェックと同じく省略します。

## アンインストール

```console
$ kobune uninstall
╭ uninstall ─────────────────────────────────────────────────────────────────╮
│ containers:                                                                │
│ myapp / main               web                                             │
│ myapp / main               db                                              │
│ myapp / feature-user-auth  web                                             │
│                                                                            │
│ storage — the data in it goes too:                                         │
│ myapp  kobune-myapp-pgdata                                                 │
│ myapp  kobune-myapp-feature-user-auth.node-modules                         │
│                                                                            │
│ files:                                                                     │
│ state, logs and the local CA  /home/u/.kobune                              │
│ shell completions             /home/u/.config/fish/completions/kobune.fish │
│ the binary                    /home/u/.local/bin/kobune                    │
│ the binary                    /home/u/.local/bin/kobuned                   │
│                                                                            │
│ needs root:                                                                │
│   stop the LaunchDaemon holding 80/443/53                                  │
│     sudo launchctl bootout system/dev.kobune.daemon                        │
│     sudo rm /Library/LaunchDaemons/dev.kobune.daemon.plist                 │
│   stop trusting the local CA                                               │
│     sudo security remove-trusted-cert -d ~/.kobune/ca/kobune-ca.crt        │
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
いない変更が入っているためです。削除は `kobune rm` が 1 つずつ行い、git が
拒否する場合は `--force` を求めます。何が残るかが分かるよう、一覧には表示
します。

**名前付きボリュームも削除しますが、その前に必ず名前を挙げます。** プロジェクト
スコープのボリュームは worktree 間で共有され、そのすべてより長く残ります。
つまり `kobune rm` の経路では決して消えず、アンインストールが唯一の削除機会
です。表示されるのはランタイムが知っている名前、つまり `docker volume ls` が
出力するものなので、取っておきたいデータベースは答える前に退避できます。探すのは
daemon の記録ではなくラベルなので、リポジトリを何か月も前に削除したプロジェ
クトのストレージも回収できます。

一覧を取得できなかったストレージ、たとえばインストール済みなのに応答しない
ランタイムのものは、「無し」として扱わず報告します。削除できなかったボリューム
も同様です。
どちらも「何かを残したアンインストール」であり、終了コードも 0 以外になります。

存在しないものは一覧に出しません。つまりこの一覧は「Kobune が置いた可能性の
ある場所」ではなく、実際にこのマシンにあるものです。`cargo build` の出力に
ついてはバイナリを削除しません。チェックアウトから `uninstall` を実行しても、
消えるのはインストール済みのものだけで、ビルド成果物は残ります。

root が必要な手順は `sudo` で実行し、パスワードを尋ねます。入力できる端末が
ないとき、つまりエージェント・パイプ・CI では、`kobune setup` と同じく
コマンドを表示するにとどめ、それ以外の削除は続行します。

## 補完

```console
$ kobune completions <bash|zsh|fish|elvish|powershell>
```

スクリプトを標準出力に書き出します。各シェルの配置先は
[インストール](../guide/installation#シェル補完) を参照してください。
インストールスクリプトを使った場合は設定済みです。

## Kobune 自体の設定に使う環境変数

| 変数 | 説明 |
| --- | --- |
| `KOBUNE_HOME` | 状態、ログ、ソケット、CA の保存先。既定値 `~/.kobune` |
| `KOBUNE_HTTP_PORT` | プロキシの HTTP ポート。既定値 80、確保できない場合は 18080。明示した場合はその値がそのまま使われます |
| `KOBUNE_HTTPS_PORT` | プロキシの HTTPS ポート。既定値 443、確保できない場合は 18443。明示した場合はその値がそのまま使われます |
| `KOBUNE_DNS_PORT` | DNS のポート。既定値 53 |
| `KOBUNE_CLOUDFLARED` | `PATH` にも主要なインストール先にも無い `cloudflared` のパス |
| `KOBUNE_CONTAINER` | Apple Container の `container` について同じもの |
| `KOBUNE_LOG` | daemon のログフィルタ。例: `debug` |
| `KOBUNE_NO_UPDATE_CHECK` | 何か値を設定すると更新チェックをしません（1 日 1 回のものと `--version` のもの、どちらも） |
