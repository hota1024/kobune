# インストール

Minato はまだ crates.io に公開していないので、ソースからビルドします。

## 必要なもの

| | |
| --- | --- |
| **Rust** | 1.85 以降 |
| **コンテナランタイム** | Docker / OrbStack / colima、または macOS 26 以降の Apple Container |
| **macOS** | 完全にサポート。Linux でも中核は動きますが launchd socket activation はありません |

デスクトップアプリは任意で、もう少し条件があります。
[デスクトップアプリ](./gui) を参照してください。

## ビルドする

```console
$ git clone https://github.com/hota1024/minato
$ cd minato
$ cargo build --release --workspace
```

`target/release` に 2 つのバイナリができます。

- `minato` — あなたが使う CLI
- `minatod` — CLI が話しかける daemon

`PATH` の通った場所に置きます。

```console
$ cp target/release/minato target/release/minatod ~/.local/bin/
```

この 2 つは一緒に配布され、隣り合っていることを前提にしています。CLI は自分の
隣を見て daemon を起動します。

## ランタイムを選ぶ

### Docker

設定は要りません。Minato は Docker API を直接叩き、`docker` CLI を呼ばないので、
CLI 自体は入っていなくても構いません。API に届きさえすれば、Docker Desktop /
OrbStack / colima のどれでも動きます。

```console
$ minato doctor
  ✓ container runtime             docker 29.4.0
```

### Apple Container

macOS 26 以降と、サービスの起動が必要です。

```console
$ container system start
```

`minato.toml` で指定します。

```toml
[runtime]
default = "apple"
```

選ぶ前に知っておくべき違いが 2 つあります。[ランタイム](./runtimes) を
参照してください。

## daemon を起動する

```console
$ minato daemon start
minatod 0.1.0 is running
```

手で叩くことはほとんどありません。どのコマンドも、daemon が動いていなければ
起動します。プロキシ・DNS・アイドル判定を持つため、常駐が必要になっています。

## 権限の要る設定

`https://web.myapp.localhost` にポート番号なしで届くには、3 つだけ root が
必要です。一度きりです。

```console
$ minato setup
The URLs need the following setup.
It requires root, so read each command before running it.

1. let launchd hold 80/443/53 (the daemon itself stays non-root)
   sudo cp ~/.minato/dev.minato.daemon.plist /Library/LaunchDaemons/…
   …

2. point *.localhost at Minato's DNS
   sudo mkdir -p /etc/resolver && printf 'nameserver 127.0.0.1\n' | sudo tee …

3. trust the local CA, so HTTPS stops warning
   sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/…
```

**`minato setup` はこれらを表示するだけで、実行はしません。** 勝手に `sudo` を
走らせるとエージェントはパスワード待ちで固まり、利用者から見れば黙って権限
昇格したことになります。内容を確認してから、自分で実行してください。

そのあと:

```console
$ minato daemon stop   # launchd が起動し直し、本来のポートを確保します
$ minato doctor
```

### 省略する場合

必須ではありません。非特権ポートを指定すれば、URL にポートが付くだけで
すべて動きます。

```console
$ export MINATO_HTTP_PORT=8080 MINATO_HTTPS_PORT=8443 MINATO_DNS_PORT=15353
$ minato daemon start
```

ただし `*.localhost` を解決させるには `/etc/resolver` の設定が要ります。これは
Minato ではなく macOS の都合です。`minato doctor` が、正しいポートを含んだ
コマンドをそのまま出します。

## 確認する

```console
$ minato doctor
```

`✓` でない行には必ず直し方が付きます。ここが赤いまま先に進まないでください。
あとで起きる分かりにくい挙動は、たいていここに行き着きます。

## 置き場所

`MINATO_HOME`（既定は `~/.minato`）に、daemon のソケット・状態ファイル・
ログ・ローカル CA・生成されたトンネル設定が置かれます。

Unix ソケットのパスは約 100 バイトまでなので、`MINATO_HOME` を深い場所には
置けません。Minato は起動時にこれを確認し、分かりにくいエラーで落ちる代わりに
そう伝えます。
