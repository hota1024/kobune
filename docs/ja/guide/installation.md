# インストール

```console
$ curl -fsSL https://minato.1024.works/install.sh | sh
```

環境に合ったアーカイブを選び、公開されている `.sha256` と照合し、`minato` と
`minatod` を `~/.local/bin` に配置します。bash / zsh / fish のうちインストール
済みのシェルには、補完スクリプトも書き込みます。

root 権限は一切必要ありません。`PATH` の設定が必要な場合は、最後に該当する 1 行
を表示します。使っているシェルに合わせて出し分けるので、fish に `export PATH`
を勧めることはありません。

## 必要なもの

| | |
| --- | --- |
| **コンテナランタイム** | Docker / OrbStack / colima のいずれか。または macOS 26 以降の Apple Container |
| **macOS** | 全機能に対応しています。Linux でも中核機能は動作しますが、launchd socket activation は利用できません |
| **Rust 1.85 以降** | [ソースからビルドする](#ビルド)場合のみ |

デスクトップアプリは任意です。追加の条件については
[デスクトップアプリ](./gui) を参照してください。

## インストールスクリプト

実行する前に中身を読んでください。[`install.sh`](https://minato.1024.works/install.sh)
は POSIX シェルで約 260 行、意外なことは何もしていません。設定は 2 つです。

| | |
| --- | --- |
| `MINATO_INSTALL_DIR` | バイナリの配置先。既定は `~/.local/bin` |
| `MINATO_NO_COMPLETIONS` | 何か値を設定すると補完スクリプトを書き込みません |

```console
$ curl -fsSL https://minato.1024.works/install.sh | MINATO_INSTALL_DIR=/usr/local/bin sh
```

インストールされるのは `nightly` ビルドです。`main` へのマージごとに差し替え
られる最新ビルドであり、リリースではありません。バージョンは付かず、内容は
予告なく変わります。

再実行すればその場で更新されます。[`minato update`](#最新に保つ) でも同じこと
ができ、こちらはシェルのパイプも 2 回のダウンロードも要りません。

## 手動でビルド済みバイナリを取得する

スクリプトが取得するのと同じアーカイブです。

| | |
| --- | --- |
| Apple Silicon | `minato-aarch64-apple-darwin.tar.gz` |
| Intel Mac | `minato-x86_64-apple-darwin.tar.gz` |
| Linux x86_64 | `minato-x86_64-unknown-linux-gnu.tar.gz` |

```console
$ gh release download nightly --repo hota1024/minato \
    --pattern 'minato-aarch64-apple-darwin.tar.gz*'
$ shasum -a 256 -c minato-aarch64-apple-darwin.tar.gz.sha256
$ tar xzf minato-aarch64-apple-darwin.tar.gz
$ cd minato-aarch64-apple-darwin
```

::: warning macOS は未署名バイナリを隔離します
```console
$ xattr -d com.apple.quarantine minato minatod
```
インストールスクリプトはこれを自動で実行します。署名は未解決のため、手動なら
このコマンドを実行するか、ソースからビルドしてください。同じ理由でデスクトップ
アプリは配布していません。未署名の `.app` は警告ではなく Gatekeeper に実行その
ものを止められます。
:::

## ビルド

Minato はまだ crates.io に公開していないため、ビルドするにはリポジトリを
クローンします。

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

## シェル補完

インストールスクリプトを使えば書き込み済みです。手動で用意する場合、あるいは
スクリプトが見つけられなかったシェル向けには次のようにします。

::: code-group
```console [fish]
$ minato completions fish > ~/.config/fish/completions/minato.fish
```

```console [zsh]
$ mkdir -p ~/.local/share/zsh/site-functions
$ minato completions zsh > ~/.local/share/zsh/site-functions/_minato
$ echo 'fpath=(~/.local/share/zsh/site-functions $fpath)' >> ~/.zshrc
```

```console [bash]
$ mkdir -p ~/.local/share/bash-completion/completions
$ minato completions bash > ~/.local/share/bash-completion/completions/minato
```
:::

fish は追加設定なしで読み込みます。zsh はディレクトリを `fpath` に加える必要が
あるため、1 行余分に書いています。bash は
[bash-completion](https://github.com/scop/bash-completion) 2.x が必要です。

`elvish` と `powershell` も指定できますが、生成器に付いてくるだけで動作確認は
していません。

## 最新に保つ

```console
$ minato update
installing 9f3c1a2…
installed 9f3c1a2

the running daemon is still the previous build.
`minato daemon stop` to replace it (launchd starts it again).
```

`update` は、実行した `minato` が置かれているディレクトリを対象に、現在の
`nightly` へ差し替えます。設定で指定した場所ではなく、いま動かしているものを
更新します。CLI と daemon は必ず一緒に入れ替えます。ビルドが揃っていないと、
両者のあいだのプロトコルが噛み合わなくなるからです。

新しいファイルは既存のファイルの隣に書き出してから rename で置き換えます。
実行中のバイナリは書き込めませんが、置き換えることはできます。そのため daemon
が動いている状態で更新しても、再起動するまでは古いビルドが動き続けます。最後の
1 行はそのことを指しています。停止すれば launchd が新しいものを起動します。

インストールせずに確認するだけなら次のようにします。

```console
$ minato update --check
a newer build is available (9f3c1a2)
run `minato update` to install it
```

### 自動チェック

1 日 1 回、コマンドが終わったあとに `nightly` の中身を GitHub へ問い合わせ、
実行中のものと違えば **stderr** に 1 行だけ表示します。

```
a newer build is available (9f3c1a2). Run `minato update`
```

コマンドの前ではなく後に実行するため、通信が遅くても待っている出力が遅れる
ことはありません。`--json` のときは完全に省略されるので、エージェントが解析
するストリームに混ざることはありません。失敗しても何も言いません。GitHub に
到達できなかったチェックには、伝えるべきことがないからです。

止めるときは環境変数を設定します。

```console
$ export MINATO_NO_UPDATE_CHECK=1
```

結果は `~/.minato/update-check.json` に 24 時間キャッシュし、その間はキャッシュ
から同じ 1 行を出し続けます。1 日に 1 度しか出ない警告は、たいてい見落とされる
からです。

ソースからビルドしたものについては、どちらとも言いません。バイナリはビルド元の
コミットを記録していますが、それが無ければ比べる相手がなく、正直に答えようが
ありません。「最新です」は推測になり、「古いです」は意図して作ったビルドから
引き剥がしてしまいます。

```console
$ minato --version
minato 0.1.0 (9f3c1a2)
```

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
