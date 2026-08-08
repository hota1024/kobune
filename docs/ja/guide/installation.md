# インストール

Minato はまだ crates.io に公開していないため、ソースからビルドします。

## 必要なもの

| | |
| --- | --- |
| **Rust** | 1.85 以降 |
| **コンテナランタイム** | Docker / OrbStack / colima のいずれか。または macOS 26 以降の Apple Container |
| **macOS** | 全機能に対応しています。Linux でも中核機能は動作しますが、launchd socket activation は利用できません |

デスクトップアプリは任意です。追加の条件については
[デスクトップアプリ](./gui) を参照してください。

## ビルド

```console
$ git clone https://github.com/hota1024/minato
$ cd minato
$ cargo build --release --workspace
```

`target/release` に 2 つのバイナリが生成されます。

- `minato` — 操作に使う CLI
- `minatod` — CLI が通信する daemon

`PATH` の通ったディレクトリに配置します。

```console
$ cp target/release/minato target/release/minatod ~/.local/bin/
```

この 2 つは同じディレクトリに置いてください。CLI は自身と同じ場所を参照して
daemon を起動します。

## コンテナランタイムの選択

### Docker

追加の設定は不要です。Minato は Docker API を直接利用し、`docker` CLI を
呼び出しません。そのため CLI 自体はインストールされていなくても、API に
到達できれば動作します。Docker Desktop、OrbStack、colima のいずれでも
構いません。

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

選択する前に把握しておくべき制約が 2 点あります。
[ランタイム](./runtimes) を参照してください。

## daemon の起動

```console
$ minato daemon start
minatod 0.1.0 is running
```

通常は手動で実行する必要はありません。いずれのコマンドも、daemon が停止して
いれば自動的に起動します。プロキシ、DNS、アイドル判定を担当するため、
常駐プロセスとして動作します。

## 管理者権限が必要な設定

`https://web.myapp.localhost` にポート番号なしでアクセスするには、3 つの設定に
管理者権限が必要です。設定は初回の 1 度だけです。

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

**`minato setup` はコマンドを表示するだけで、実行はしません。** 自動で `sudo`
を実行すると、エージェントはパスワード入力待ちで停止し、利用者から見れば
黙って権限昇格が行われたことになります。内容を確認したうえで、手動で実行して
ください。

実行後は次のようにします。

```console
$ minato daemon stop   # launchd が起動し直し、標準ポートを確保します
$ minato doctor
```

### 設定を省略する場合

この設定は必須ではありません。非特権ポートを指定すれば、URL にポート番号が
付く点を除いてすべて動作します。

```console
$ export MINATO_HTTP_PORT=8080 MINATO_HTTPS_PORT=8443 MINATO_DNS_PORT=15353
$ minato daemon start
```

ただし `*.localhost` を解決させるには `/etc/resolver` の設定が必要です。
これは Minato ではなく macOS 側の仕様です。`minato doctor` が、指定した
ポートを含んだコマンドをそのまま出力します。

## 動作確認

```console
$ minato doctor
```

`✓` 以外の行には、必ず対処方法が併記されます。ここに問題が残ったまま先に
進まないでください。後から発生する原因の分かりにくい不具合は、多くの場合
ここに起因します。

## ファイルの配置場所

`MINATO_HOME`（既定値 `~/.minato`）に、daemon のソケット、状態ファイル、
ログ、ローカル CA、生成されたトンネル設定が保存されます。

Unix ソケットのパスは約 100 バイトまでという制限があるため、`MINATO_HOME` に
深い階層のディレクトリは指定できません。Minato は起動時にこれを検証し、
原因の分かりにくいエラーで失敗する代わりに、その旨を明示します。
