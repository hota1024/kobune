# 基本操作

実際に使用頻度の高いコマンドを、おおよその使用順に紹介します。

## コマンドの対象範囲

ほとんどのコマンドは、対象となる workspace を特定する必要があります。判定
方法は 2 つです。

1. **カレントディレクトリ。** worktree の内側であれば、その worktree が対象に
   なります。
2. **`-w, --workspace`。** リポジトリ内のどこからでも、対象を明示的に指定
   できます。

```console
$ cd ../myapp.wt/feature-auth && minato status   # この worktree が対象
$ minato status -w feature-auth                  # 同じ対象を明示的に指定
```

workspace 名はブランチ名をサニタイズしたもので、`feature/user-auth` であれば
`feature-user-auth` になります。対応関係は `minato ls` で確認できます。

## 作業を開始する

```console
$ minato new feature/user-auth
```

worktree を作成し、環境を起動して URL を表示します。

```console
$ minato new hotfix/login --base v1.2.0   # 分岐元を指定する
$ minato new feature/x --path ../elsewhere
$ minato new feature/x --no-start         # worktree の作成のみ
```

ブランチがすでに存在する場合は、新規作成せずチェックアウトします。

`git worktree add` で作成した worktree も認識されます。その worktree で最初に
コマンドを実行した時点で登録されるため、作成方法を指摘されることはありません。

## 状態を確認する

```console
$ minato ls        # 全 workspace と稼働中のサービス数
$ minato status    # 対象 workspace の詳細（状態、URL、アドレス）
```

サービスの状態は次の 4 つです。

| 状態 | 意味 |
| --- | --- |
| `ready` | 稼働中で、リクエストに応答している |
| `starting` | コンテナは起動したが、まだ応答していない |
| `stopped` | 停止中。リクエストが来れば起動する |
| `failed` | 起動を試みて失敗した。`reason` に理由が入る |

`stopped` は異常ではありません。使用されていない環境の正常な状態です。

## URL を取得する

```console
$ minato url          # 最初にアクセス可能なサービス
$ minato url web      # サービス名を指定
```

出力は 1 行のみのため、そのまま埋め込めます。

```console
$ curl -sS --fail-with-body "$(minato url web)/api/health"
```

**URL は直接記述せず、このコマンドで取得してください。** 再起動しても URL は
変わりませんが、内部のポート番号は変わります。

## 起動と停止

```console
$ minato up               # この workspace のすべてのサービス
$ minato up web api       # 指定したサービスとその依存先のみ
$ minato down             # この workspace を停止
$ minato down --all       # プロジェクト内の全 workspace を停止
```

`up` は稼働中のコンテナには変更を加えないため、複数回実行しても問題ありません。
一方、**停止中**のコンテナは削除して再作成します。設定変更を反映するためで、
数秒の追加時間はかかりますが、変更が反映されない状態を調査するコストよりは
小さいはずです。

なお `up` の実行はほとんどの場合不要です。停止中のサービスはリクエストで
起動します。

## ログ

```console
$ minato logs                  # この workspace の全サービス
$ minato logs web              # 特定のサービス
$ minato logs web -n 100       # 末尾 100 行
$ minato logs web -f           # 継続的に出力
```

装飾を含まないため、grep やパイプでそのまま処理できます。stdout と stderr は
分離されたままです。

複数サービスを対象にした場合、出力は混在しますが、行ごとにどのサービスの
ものかが示されます。

## コンテナ内でコマンドを実行する

```console
$ minato exec web -- npm test
$ minato exec web -- sh
```

**終了コードは実行したコマンドのものがそのまま返ります。**

```console
$ minato exec web -- npm test && echo "passed"
```

TTY は要求しません。入力待ちになるコマンドはプロンプトを表示せず停止するため、
`--yes` のような非対話用のオプションを指定してください。

## 環境変数

```console
$ minato env ls                          # 定義元の層も表示される
$ minato env get DATABASE_URL            # 値を 1 行で出力（パイプ用）
$ minato env set API_KEY=xxx             # この worktree のみ
$ minato env set LOG_LEVEL=debug --scope project
$ minato env unset API_KEY
```

変更は稼働中のコンテナには反映されません。`minato down && minato up` で反映
されます。CLI も実行後にその旨を表示します。

詳細は [環境変数](./environment-variables) を参照してください。

## 後片付け

```console
$ minato rm -w feature-user-auth        # worktree とコンテナを削除
$ minato rm -w feature-user-auth -f     # 未コミットの変更があっても削除
```

ブランチは残ります。共有サービス（`scope = "project"`）も、他の worktree が
使用しているため残ります。

## daemon の操作

```console
$ minato daemon status
$ minato daemon start
$ minato daemon stop
```

通常は使用しません。いずれのコマンドも、daemon が停止していれば自動的に
起動します。停止するとプロキシと DNS も止まるため、再起動するまで URL は
解決しなくなります。コンテナ自体は稼働を続けます。

launchd を設定している場合、`daemon stop` の直後に launchd が再起動します。
これは意図した動作で、80/443 番ポートを確保したまま設定を再読み込みする手段
です。

## 問題が起きたとき

`docker` を直接操作する前に、次の順序で確認してください。

```console
$ minato status      # どの状態にあるか
$ minato logs web    # アプリケーション側のエラー
$ minato doctor      # 環境側の問題
```

詳細は [困ったときは](./troubleshooting) を参照してください。
