# 困ったときは

この順で進めてください。推測で `docker` に手を伸ばすと、状態が食い違います。

```console
$ minato status      # サービスはどういう状態か
$ minato logs web    # アプリは何と言っているか
$ minato doctor      # 環境は何と言っているか
```

`minato doctor` は `✓` でない行すべてに直し方を出します。

## よくある症状

### `curl` が終了コード 60 で落ちる

ローカル CA が信頼されていません。

```console
$ minato doctor
  ! local CA trust    not trusted; browsers and curl will warn over HTTPS
    sudo security add-trusted-cert -d -r trustRoot \
      -k /Library/Keychains/System.keychain ~/.minato/ca/minato-ca.crt
```

最初にぶつかるものとして一番多いです。素の `curl -s` はこのエラーを握り潰し、
空の応答のように見えるので `-sS --fail-with-body` を使ってください。

### URL が解決しない

```console
$ minato doctor
  ✗ DNS resolver (/etc/resolver/localhost)   not installed
```

macOS は `*.localhost` を自分では解決しません。直し方は出力にあり、daemon の
動かし方に合ったポートが入っています。

### プロキシが 404 を返す

```
Minato: there is no environment behind `web.feat-1.myapp.localhost`.
Run `minato ls` to see which workspaces are up.
```

ホスト名が登録されたサービスと一致しません。打ち間違い、リネーム後の古い URL、
`expose = false` のいずれかがほとんどです。`minato url` で取り直してください。

### 502 が返る

サービスは登録されているのに応答していません。起動したあと落ちたか、
`minato.toml` と違うポートで待ち受けています。

```console
$ minato logs web -n 50
$ minato status          # ready か、failed か
```

`port` がアプリの実際の bind と一致しているか、そして `127.0.0.1` ではなく
`0.0.0.0` に bind しているか確認してください。コンテナ内でループバックに
bind したサーバは、外から届きません。

### いつまでも起動が終わらない

```console
$ minato logs web -f
```

Minato は 15 秒待ってから、警告を出して先に進みます。依存解決やコンパイルを
する初回起動はそれより長くかかります。コンテナはまだ立ち上がり中です。

`health` を書くと精度が上がります。無い場合、判定は「TCP 接続が通った」だけ
です。

### 設定を変えたのに効かない

すでに動いているコンテナは変更を拾いません。

```console
$ minato down && minato up
```

`minato.toml` でも環境変数でも同じです。

### 再起動したら何も動かない

```console
$ minato daemon status
$ minato doctor
```

LaunchDaemon を設置していないと、daemon は自分では戻ってきません。
`minato setup` が設置方法を出します。

### 「the Unix socket path is too long」

`MINATO_HOME` が深すぎます。ソケットのパスは約 100 バイトまでです。もっと
浅い場所に —— 既定の `~/.minato` で問題ありません。

### 別のアプリにリクエストが届く

```console
$ minato doctor
  ✗ listening addresses   [::1] could not be held. *.localhost resolves to
                          both, so requests to that address reach another
                          process
```

ループバックのどちらかのアドレスを別のプロセスが持っています。`*.localhost` は
`::1` と `127.0.0.1` の両方に解決され、クライアントは IPv6 を優先するので、
片方しか押さえていないと別の場所に流れます。相手のプロセスを止めるか、
`MINATO_HTTP_PORT` で Minato を動かしてください。

## Apple Container

### `MINATO_HOST_<SERVICE>` が設定されていない

このサービスが起動した時点で peer が動いていませんでした。`depends_on` を
足して、先に起動させてください。

変数が無いのは意図的です。Apple Container にはコンテナ間 DNS が無いので、
ホスト名を渡しても解決されず、間違った問題を探すことになります。
[ランタイム](./runtimes) を参照。

### `container system status` が running でない

```console
$ container system start
```

Docker と同様、Minato が代わりに起動することはありません。

## もっと深く見る

```console
$ tail -f ~/.minato/logs/minatod.log
$ MINATO_LOG=debug minatod          # フォアグラウンドで
```

どうしてもコンテナを直接見るなら、読むだけにしてください。

```console
$ docker ps --filter label=dev.minato.managed=1
$ container ls --all
```

Minato が管理するものにはすべて `dev.minato.*` ラベルが付いており、それが
状態の正です。裏でコンテナを変更すると、この 2 つが食い違います。
