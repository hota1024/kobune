# 日々の使い方

実際に使うコマンドを、だいたい使う順に。

## コマンドがどこに効くか

ほとんどのコマンドは、どの workspace のことか知る必要があります。決め方は
2 つです。

1. **いまいるディレクトリ。** worktree の中なら、その worktree が対象です。
2. **`-w, --workspace`。** リポジトリのどこからでも明示的に指定します。

```console
$ cd ../myapp.wt/feature-auth && minato status   # この worktree
$ minato status -w feature-auth                  # 同じものを、どこからでも
```

workspace 名はブランチ名をサニタイズしたもので、`feature/user-auth` は
`feature-user-auth` になります。`minato ls` に両方出ます。

## 作業を始める

```console
$ minato new feature/user-auth
```

worktree を作り、環境を起動し、URL を出します。

```console
$ minato new hotfix/login --base v1.2.0   # 分岐元を指定
$ minato new feature/x --path ../elsewhere
$ minato new feature/x --no-start         # worktree だけ
```

ブランチが既にあれば、作らずにチェックアウトします。

素の `git worktree add` で作った worktree も拾います。その中で最初にコマンドを
実行したときに登録されるので、「worktree の作り方を間違えた」と言われることは
ありません。

## 状況を見る

```console
$ minato ls        # 全 workspace と、いくつ動いているか
$ minato status    # この workspace の詳細。状態・URL・アドレス
```

サービスの状態は 4 つです。

| 状態 | 意味 |
| --- | --- |
| `ready` | 動いていて、応答している |
| `starting` | コンテナは上がったが、まだ応答しない |
| `stopped` | 止まっている。リクエストが来れば起動する |
| `failed` | 試して失敗した。`reason` に理由がある |

`stopped` は問題ではありません。誰も使っていない環境はそうあるべき姿です。

## URL を取る

```console
$ minato url          # 最初の到達可能なサービス
$ minato url web      # 名前を指定
```

1 行だけなので、そのまま埋め込めます。

```console
$ curl -sS --fail-with-body "$(minato url web)/api/health"
```

**URL は書くのではなく、聞いてください。** 再起動しても変わりませんが、裏の
ポートは変わります。

## 起動と停止

```console
$ minato up               # この workspace のすべて
$ minato up web api       # これらと、その依存先だけ
$ minato down             # この workspace を止める
$ minato down --all       # プロジェクト内のすべての workspace
```

`up` は動いているコンテナに触れないので、2 回叩いても害はありません。
*停止中* のコンテナは削除して作り直すので、設定変更が反映されます。数秒
かかりますが、「直したのに効かない」と悩むよりは安いはずです。

そもそも `up` はあまり必要ありません。停止中のサービスはリクエストで起きます。

## ログ

```console
$ minato logs                  # この workspace の全サービス
$ minato logs web              # 1 つ
$ minato logs web -n 100       # 末尾 100 行
$ minato logs web -f           # 追い続ける
```

装飾がないので grep にもパイプにもかけられます。stdout と stderr は分かれた
ままです。

複数サービスなら行は混ざり、それぞれどのサービスのものか印が付きます。

## コンテナの中でコマンドを実行する

```console
$ minato exec web -- npm test
$ minato exec web -- sh
```

**終了コードはコマンドのものがそのまま返ります。**

```console
$ minato exec web -- npm test && echo "passed"
```

TTY は要求しません。入力を待つコマンドはプロンプトを出さずに固まるので、
`--yes` のようなフラグを渡してください。

## 環境変数

```console
$ minato env ls                          # どの層の値かも出る
$ minato env get DATABASE_URL            # 1 つの値。パイプ用
$ minato env set API_KEY=xxx             # この worktree
$ minato env set LOG_LEVEL=debug --scope project
$ minato env unset API_KEY
```

変更は動いているコンテナには届きません。`minato down && minato up` で反映され、
CLI もそう促します。

[環境変数](./environment-variables) を参照。

## 片付ける

```console
$ minato rm -w feature-user-auth        # worktree とコンテナ
$ minato rm -w feature-user-auth -f     # 未コミットの変更があっても
```

ブランチは残ります。共有サービス（`scope = "project"`）も、他の worktree が
使っているので残ります。

## daemon

```console
$ minato daemon status
$ minato daemon start
$ minato daemon stop
```

触ることはほとんどありません。どのコマンドも、止まっていれば起動します。
止めるとプロキシと DNS も止まるので、戻るまで URL は解決しません。コンテナ
自体は動き続けます。

launchd を設置してある場合、`daemon stop` の直後に launchd が起動し直します。
これは意図的で、80/443 を確保したまま新しい設定を読み直す手段です。

## うまくいかないとき

`docker` に手を伸ばす前に、この順で。

```console
$ minato status      # どういう状態か
$ minato logs web    # アプリは何と言っているか
$ minato doctor      # 環境は何と言っているか
```

[困ったときは](./troubleshooting) を参照。
