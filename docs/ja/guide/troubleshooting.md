# 困ったときは

次の順序で確認してください。推測で `docker` を直接操作すると、状態が食い違う
原因になります。

```console
$ kobune status      # サービスの状態を確認する
$ kobune logs web    # アプリケーション側のエラーを確認する
$ kobune doctor      # 環境側の問題を確認する
```

`kobune doctor` は、`✓` 以外のすべての項目に対処方法を表示します。

## よくある症状

### `curl` が終了コード 60 で失敗する

ローカル CA が信頼されていません。

```console
$ kobune doctor
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
$ kobune doctor
│ …
│ ✗  DNS resolver (/etc/resolver/localhost)  not installed
│ …
```

macOS は `*.localhost` を標準では解決しません。対処方法は出力に含まれており、
daemon の起動方法に応じた正しいポート番号が入っています。

### プロキシが 404 を返す

```
Kobune: there is no environment behind `web.feat-1.myapp.localhost`.
Run `kobune ls` to see which workspaces are up.
```

ホスト名が登録済みのサービスと一致していません。入力ミス、リネーム後の古い
URL、`expose = false` の指定のいずれかが大半です。`kobune url` で URL を
取得し直してください。

### 502 が返る

サービスは登録されていますが、応答していません。起動後に停止したか、
`kobune.toml` の指定とは異なるポートで待ち受けています。

```console
$ kobune logs web -n 50
$ kobune status          # ready か failed か
```

`port` の指定がアプリケーションの実際のバインド先と一致しているか、そして
`127.0.0.1` ではなく `0.0.0.0` にバインドしているかを確認してください。
コンテナ内でループバックにバインドしたサーバには、外部から到達できません。

### 起動が完了しない

```console
$ kobune logs web -f
```

Kobune は 15 秒待機したあと、警告を出力して処理を継続します。依存関係の解決や
コンパイルを行う初回起動はそれより時間がかかるため、コンテナはまだ起動処理中
です。

`health` を設定すると判定の精度が上がります。未設定の場合、判定は TCP 接続の
可否のみです。

### 設定を変更しても反映されない

稼働中のコンテナは変更を読み込みません。

```console
$ kobune down && kobune up
```

`kobune.toml` の変更でも環境変数の変更でも同様です。

### 再起動後に動作しない

```console
$ kobune daemon status
$ kobune doctor
```

LaunchDaemon を設定していない場合、daemon は自動的には復帰しません。
`kobune setup` が、その設定を実行するか確認します。

### LaunchDaemon は設定済みなのにジョブが起動しない

```console
$ kobune doctor
│ !  launchd socket activation  inactive, though launchd has the LaunchDaemon
```

launchd 以外の方法で起動した daemon が Unix ソケットを保持していると、launchd
のジョブは起動時にそれを見つけて終了します。正常終了したジョブは再起動されない
ため、以降もフォールバックのポートで動作し続けます。設定の失敗ではありません。

```console
$ kobune daemon restart
```

停止でソケットを明け渡し、起動で :80——launchd が保持しているポート——に到達する
ため、立ち上がるのは launchd のジョブで、80・443・53 を保持します。root は不要
です（`launchctl kickstart` には必要になります）。`kobune doctor` と
`kobune setup` も同じコマンドを提示します。

停止するだけでも、次に届いたリクエストがジョブを起こすため最終的には復旧します
が、それまでのあいだ daemon は不在で、`kobune daemon status` も停止と報告
します。

これで復旧しない場合は、restart 自体がそう報告します。:80 に到達しても
launchd がそこにいなかった——別のプロセスがポートを保持している、あるいは
ジョブのソケットが bind できていない——ということで、起動は独自の daemon に
フォールバックしています。終了コードは 0 以外になり、次に何をすべきかも表示
されるため、`kobune doctor` を実行しなくても——スクリプトからでも——判断
できます。

```console
$ sudo launchctl kickstart -k system/dev.kobune.daemon
```

**再インストールは解決になりません。** launchd は登録済みのラベルに対する 2 度目
の `bootstrap` を `Bootstrap failed: 5: Input/output error` として拒否するため、
`kobune setup` もこの状態では再インストールを提示しません。

### launchd のジョブが別の `KOBUNE_HOME` のものである

```console
$ kobune doctor
│ !  launchd socket activation  inactive: launchd's job serves KOBUNE_HOME=/Users/hotaka/.kobune, and this daemon runs under /tmp/kobune-elsewhere
```

plist にはインストール時の home が書き込まれており、いま使っている home は
それとは別のものです。launchd が 80・443・53 を保持しているのは登録済みの
ジョブのためで、そのジョブが対象にしているのは別の home です。こちらから
実行できるコマンドでそれを奪うことはできません。

```console
$ kobune daemon restart
✗ error: started a daemon outside launchd, so 80 and 443 are out and no URL will answer
  hint: launchd's job serves KOBUNE_HOME=/Users/hotaka/.kobune, so those ports are held for a daemon that is not this one. Point KOBUNE_HOME there to reach it, or keep the ports this daemon fell back to
```

残りの 2 つも同様です。`launchctl kickstart` は同じ home 向けの同じジョブを
起動し直すだけで、`kobune setup` はこの状態では launchd のステップを提示
しません。登録済みのラベルに対する 2 度目の `bootstrap` は
`Input/output error` になるためで、状態だけを伝えて何もしません。

そのジョブの home を `KOBUNE_HOME` に指定すれば、ポートを保持している daemon
に到達できます。そうでなければこれは意図的な 2 つ目のインスタンスであり、
フォールバックのポートのまま——URL にもそのポートが付いたまま——動作します。

### 「the Unix socket path is too long」と表示される

`KOBUNE_HOME` の階層が深すぎます。ソケットのパスは約 100 バイトまでです。
より浅い階層を指定してください。既定値の `~/.kobune` であれば問題ありません。

### 別のアプリケーションにリクエストが届く

```console
$ kobune doctor
│ …
│ ✗  listening addresses  the HTTPS proxy could not hold [::1]. *.localhost
│                         resolves to both families and clients prefer IPv6,
│                         so requests to that address reach another process
│ …
```

ループバックアドレスの一方を別のプロセスが使用しています。`*.localhost` は
`::1` と `127.0.0.1` の両方に解決され、クライアントは IPv6 を優先するため、
一方しか確保できていないと別のプロセスにリクエストが到達します。当該プロセスを
停止するか、`KOBUNE_HTTP_PORT` で Kobune のポートを変更してください。

**HTTP と HTTPS は個別に報告されます。** 両者は独立して bind するため、HTTP は
両系統を確保できていて HTTPS だけ一方を失っている、という状態があり得ます。
確認すべきなのはメッセージに名前が出ているほうです。

## Apple Container 固有の問題

### `KOBUNE_HOST_<SERVICE>` が設定されていない

このサービスの起動時点で、参照先のサービスが稼働していませんでした。`depends_on`
を追加して、先に起動させてください。

変数を未設定のままにしているのは意図的です。Apple Container にはコンテナ間
DNS が存在しないため、ホスト名を渡しても解決されず、原因の特定が難しくなり
ます。[ランタイム](./runtimes) を参照してください。

### `container system status` が running にならない

```console
$ container system start
```

Docker と同様に、Kobune が代わりに起動することはありません。

## 詳細な調査

```console
$ tail -f ~/.kobune/logs/kobuned.log
$ KOBUNE_LOG=debug kobuned          # フォアグラウンドで実行する
```

コンテナを直接確認する必要がある場合は、参照のみに留めてください。

```console
$ docker ps --filter label=dev.kobune.managed=1
$ container ls --all
```

Kobune が管理するリソースにはすべて `dev.kobune.*` ラベルが付与されており、
これが状態の正です。Kobune を介さずにコンテナを変更すると、両者の状態が
食い違います。
