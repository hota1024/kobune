# 困ったときは

次の順序で確認してください。推測で `docker` を直接操作すると、状態が食い違う
原因になります。

```console
$ minato status      # サービスの状態を確認する
$ minato logs web    # アプリケーション側のエラーを確認する
$ minato doctor      # 環境側の問題を確認する
```

`minato doctor` は、`✓` 以外のすべての項目に対処方法を表示します。

## よくある症状

### `curl` が終了コード 60 で失敗する

ローカル CA が信頼されていません。

```console
$ minato doctor
│ …
│ !  local CA trust  not trusted; browsers and curl will warn over HTTPS
│
│ to fix:
│ ! local CA trust
│   sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/…
```

最初に遭遇する問題として最も多いものです。`curl -s` だけではこのエラーが
握り潰され、空の応答が返ったように見えるため、`-sS --fail-with-body` を
指定してください。

### URL が解決されない

```console
$ minato doctor
│ …
│ ✗  DNS resolver (/etc/resolver/localhost)  not installed
│ …
```

macOS は `*.localhost` を標準では解決しません。対処方法は出力に含まれており、
daemon の起動方法に応じた正しいポート番号が入っています。

### プロキシが 404 を返す

```
Minato: there is no environment behind `web.feat-1.myapp.localhost`.
Run `minato ls` to see which workspaces are up.
```

ホスト名が登録済みのサービスと一致していません。入力ミス、リネーム後の古い
URL、`expose = false` の指定のいずれかが大半です。`minato url` で URL を
取得し直してください。

### 502 が返る

サービスは登録されていますが、応答していません。起動後に停止したか、
`minato.toml` の指定とは異なるポートで待ち受けています。

```console
$ minato logs web -n 50
$ minato status          # ready か failed か
```

`port` の指定がアプリケーションの実際のバインド先と一致しているか、そして
`127.0.0.1` ではなく `0.0.0.0` にバインドしているかを確認してください。
コンテナ内でループバックにバインドしたサーバには、外部から到達できません。

### 起動が完了しない

```console
$ minato logs web -f
```

Minato は 15 秒待機したあと、警告を出力して処理を継続します。依存関係の解決や
コンパイルを行う初回起動はそれより時間がかかるため、コンテナはまだ起動処理中
です。

`health` を設定すると判定の精度が上がります。未設定の場合、判定は TCP 接続の
可否のみです。

### 設定を変更しても反映されない

稼働中のコンテナは変更を読み込みません。

```console
$ minato down && minato up
```

`minato.toml` の変更でも環境変数の変更でも同様です。

### 再起動後に動作しない

```console
$ minato daemon status
$ minato doctor
```

LaunchDaemon を設定していない場合、daemon は自動的には復帰しません。
`minato setup` が、その設定を実行するか確認します。

### 「the Unix socket path is too long」と表示される

`MINATO_HOME` の階層が深すぎます。ソケットのパスは約 100 バイトまでです。
より浅い階層を指定してください。既定値の `~/.minato` であれば問題ありません。

### 別のアプリケーションにリクエストが届く

```console
$ minato doctor
│ …
│ ✗  listening addresses  [::1] could not be held. *.localhost resolves to both,
│                         so requests to that address reach another process
│ …
```

ループバックアドレスの一方を別のプロセスが使用しています。`*.localhost` は
`::1` と `127.0.0.1` の両方に解決され、クライアントは IPv6 を優先するため、
一方しか確保できていないと別のプロセスにリクエストが到達します。当該プロセスを
停止するか、`MINATO_HTTP_PORT` で Minato のポートを変更してください。

## Apple Container 固有の問題

### `MINATO_HOST_<SERVICE>` が設定されていない

このサービスの起動時点で、参照先のサービスが稼働していませんでした。`depends_on`
を追加して、先に起動させてください。

変数を未設定のままにしているのは意図的です。Apple Container にはコンテナ間
DNS が存在しないため、ホスト名を渡しても解決されず、原因の特定が難しくなり
ます。[ランタイム](./runtimes) を参照してください。

### `container system status` が running にならない

```console
$ container system start
```

Docker と同様に、Minato が代わりに起動することはありません。

## 詳細な調査

```console
$ tail -f ~/.minato/logs/minatod.log
$ MINATO_LOG=debug minatod          # フォアグラウンドで実行する
```

コンテナを直接確認する必要がある場合は、参照のみに留めてください。

```console
$ docker ps --filter label=dev.minato.managed=1
$ container ls --all
```

Minato が管理するリソースにはすべて `dev.minato.*` ラベルが付与されており、
これが状態の正となります。Minato を介さずにコンテナを変更すると、両者の状態が
食い違います。
