# インストール

```console
$ curl -fsSL https://minato.1024.works/install.sh | sh
```

環境に合ったアーカイブを選び、公開されている `.sha256` と照合し、`kobune` と
`kobuned` を `~/.local/bin` に配置します。bash / zsh / fish のうちインストール
済みのシェルには、補完スクリプトも書き込みます。

root 権限は一切必要ありません。`PATH` の設定が必要な場合は、いま使っている
シェルの書き方で 1 行だけ表示します。fish なら `fish_add_path` を示すので、
fish が受け付けない `export` 行を渡されることはありません。

## 何をインストールすることになるか {#nightly}

リリースはまだ 1 つもありません。`nightly` は `main` のビルドで、マージの
たびに差し替えられます。上のコマンドが取得するのもこれです。

`kobune --version` は、クレートのバージョンとビルド元のコミットを表示します
（`0.1.0 (a1b2c3d)`）。ビルドを識別しているのはコミットのほうです。前に付いて
いる番号はリリースされたことがなく、中身が変わっても据え置かれます。

したがって、一般に「安定版」と呼ばれるものではありません。フラグの名前が
変わることも、既定値が変わることもあり、あとから読めるのは、その変更をした
コミットだけです。
[CHANGELOG.md](https://github.com/hota1024/kobune/blob/main/CHANGELOG.md) は、
そうでなくなる日のために置いてあります。

未完成という意味ではありません。Firecracker を除くマイルストーンは着地して
おり、このページで用意する環境は動きます。まだないのは、動かないでいてくれる
バージョンのほうです。

## 必要なもの

| 必要なもの | 補足 |
| --- | --- |
| **コンテナランタイム** | Docker / OrbStack / colima のいずれか。または macOS 26 以降の Apple Container |
| **macOS** | 全機能に対応しています。Linux でも中核機能は動作しますが、launchd socket activation は利用できません |
| **Rust 1.88 以降** | [ソースからビルドする](#ビルド)場合のみ |

デスクトップアプリは任意です。追加の条件については
[デスクトップアプリ](./gui) を参照してください。

## インストールスクリプト

実行する前に中身を読んでください。[`install.sh`](https://minato.1024.works/install.sh)
は POSIX シェルで約 260 行、意外なことは何もしていません。設定は 2 つです。

| | |
| --- | --- |
| `KOBUNE_INSTALL_DIR` | バイナリの配置先。既定は `~/.local/bin` |
| `KOBUNE_NO_COMPLETIONS` | 何か値を設定すると補完スクリプトを書き込みません |

```console
$ curl -fsSL https://minato.1024.works/install.sh | KOBUNE_INSTALL_DIR=/usr/local/bin sh
```

### PATH の設定

配置先が `PATH` に入っていない場合、追加する方法を表示します。示すのは 1 つ、
いま使っているシェルの分だけです。

::: code-group
```console [fish]
fish_add_path ~/.local/bin
```

```console [zsh]
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc
. ~/.zshrc
```

```console [bash]
# macOS は ~/.bash_profile、Linux は ~/.bashrc。ログインシェルは前者しか
# 読まず、macOS のターミナルはログインシェルとして起動します。
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bash_profile
. ~/.bash_profile
```

```console [tcsh]
echo 'setenv PATH $HOME/.local/bin:$PATH' >> ~/.tcshrc
source ~/.tcshrc
```

```console [nushell]
# $nu.config-path に追記
$env.PATH = ($env.PATH | prepend '~/.local/bin')
```

```console [elvish]
# ~/.config/elvish/rc.elv に追記
set paths = ['~/.local/bin' $@paths]
```

```console [powershell]
# $PROFILE に追記
$env:PATH = "$HOME/.local/bin" + [IO.Path]::PathSeparator + $env:PATH
```
:::

判定には `$SHELL` ではなくプロセスツリーを使います。`$SHELL` はログイン
シェルであり、zsh でログインしてから fish を起動した時点で別のものになります。
fish で `export PATH` を渡されると、そのまま設定ファイルに貼られて何か月も
壊れたままになりがちです。`ksh` / `mksh` / `dash` も判別し、`~/.profile` を
案内します。

判定できなかったときは推測せず、すべてのシェルの方法を並べて表示します。

インストールされるのは `nightly` ビルドです。`main` へのマージごとに差し替え
られる最新ビルドであり、リリースではありません。バージョンは付かず、内容は
予告なく変わります。

再実行すればその場で更新されます。[`kobune update`](#最新に保つ) でも同じこと
ができ、こちらはシェルのパイプも 2 回のダウンロードも要りません。

## 手動でビルド済みバイナリを取得する

スクリプトが取得するのと同じアーカイブです。

| | |
| --- | --- |
| Apple Silicon | `kobune-aarch64-apple-darwin.tar.gz` |
| Intel Mac | `kobune-x86_64-apple-darwin.tar.gz` |
| Linux x86_64 | `kobune-x86_64-unknown-linux-gnu.tar.gz` |

```console
$ gh release download nightly --repo hota1024/kobune \
    --pattern 'kobune-aarch64-apple-darwin.tar.gz*'
$ shasum -a 256 -c kobune-aarch64-apple-darwin.tar.gz.sha256
$ tar xzf kobune-aarch64-apple-darwin.tar.gz
$ cd kobune-aarch64-apple-darwin
```

::: warning macOS は未署名バイナリを隔離します
```console
$ xattr -d com.apple.quarantine kobune kobuned
```
インストールスクリプトはこれを自動で実行します。署名は未解決のため、手動なら
このコマンドを実行するか、ソースからビルドしてください。同じ理由でデスクトップ
アプリは配布していません。未署名の `.app` は警告ではなく Gatekeeper に実行その
ものを止められます。
:::

## ビルド

Kobune はまだ crates.io に公開していないため、ビルドするにはリポジトリを
クローンします。

```console
$ git clone https://github.com/hota1024/kobune
$ cd kobune
$ cargo build --release --workspace
```

`target/release` に 2 つのバイナリが生成されます。

- `kobune` — 操作に使う CLI
- `kobuned` — CLI が通信する daemon

`PATH` の通ったディレクトリに配置します。

```console
$ cp target/release/kobune target/release/kobuned ~/.local/bin/
```

この 2 つは同じディレクトリに置いてください。CLI は自身と同じ場所を参照して
daemon を起動します。

## シェル補完

インストールスクリプトを使えば書き込み済みです。手動で用意する場合、あるいは
スクリプトが見つけられなかったシェル向けには次のようにします。

::: code-group
```console [fish]
$ kobune completions fish > ~/.config/fish/completions/kobune.fish
```

```console [zsh]
$ mkdir -p ~/.local/share/zsh/site-functions
$ kobune completions zsh > ~/.local/share/zsh/site-functions/_kobune
$ echo 'fpath=(~/.local/share/zsh/site-functions $fpath)' >> ~/.zshrc
```

```console [bash]
$ mkdir -p ~/.local/share/bash-completion/completions
$ kobune completions bash > ~/.local/share/bash-completion/completions/kobune
```
:::

fish は追加設定なしで読み込みます。zsh はディレクトリを `fpath` に加える必要が
あるため、1 行余分に書いています。bash は
[bash-completion](https://github.com/scop/bash-completion) 2.x が必要です。

`elvish` と `powershell` も指定できますが、生成器に付いてくるだけで動作確認は
していません。

## 最新に保つ

```console
$ kobune update
› installing 9f3c1a2…
╭ update ────────────────────────────────────────────────────────────────╮
│ installed  9f3c1a2                                                     │
│                                                                        │
│ › the daemon is still the previous build, so run kobune daemon restart │
╰────────────────────────────────────────────────────────────────────────╯
```

`update` は、実行した `kobune` が置かれているディレクトリを対象に、現在の
`nightly` へ差し替えます。設定で指定した場所ではなく、いま動かしているものを
更新します。CLI と daemon は必ず一緒に入れ替えます。ビルドが揃っていないと、
両者のあいだのプロトコルが噛み合わなくなるからです。

新しいファイルは既存のファイルの隣に書き出してから rename で置き換えます。
実行中のバイナリは書き込めませんが、置き換えることはできます。そのため daemon
が動いている状態で更新しても、再起動するまでは古いビルドが動き続けます。最後の
1 行はそのことを指しています。どのマシンでも同じことを言い、違うのは再起動の
結果です。daemon の起動は、launchd がジョブを持っていればまず launchd に頼み
ます。そのため、そこで再起動すれば launchd が起動した daemon が残ります。80 と
443 を保持したまま、いま入ったビルドで動くものです。LaunchDaemon が無いマシン
では頼む相手がいないので、直接起動された daemon が、それまでと同じ代替ポートで
動きます。

この 1 行は **状態から導いたもので、常に出るわけではありません**。daemon が
動いていなければ入れ替えるものはなく、パネルは入れたビルドだけを伝えます。

インストールせずに確認するだけなら次のようにします。

```console
$ kobune update --check
╭ update ─────────────────────────╮
│ available  9f3c1a2              │
│ running    c7282b8              │
│                                 │
│ › install it with kobune update │
╰─────────────────────────────────╯
```

### 新しいビルドが残した作業

上のパネルが答えられるのは、置き換えられる側のビルドについてだけです。
リポジトリの Skill が新しいものと一致しているか、LaunchDaemon が新しいビルドの
書く形になっているか。これらに答えられるのは、入った側のビルドだけです。
そこで、そのビルドが最初に実行されたときに自分で答えます。

```console
$ kobune url web
https://web.myapp.localhost
› kobune changed to 9f3c1a2 since the last run
› the daemon is not this build, so run kobune daemon restart
› this repository's Skill is not this build's, so run kobune skill install --force
```

どの行も、このマシンの状態がそう言っているから出ています。daemon が応答して、
返ってきたバージョンがこのビルドのものではなかった、
`.claude/skills/kobune/SKILL.md` がこのバイナリの持つものと違う、入っている
plist が古い形で書かれている。推測はしません。Skill を一度も入れていない
リポジトリに勧めることはなく、記録が始まる前に書かれた plist を古いと
決めつけることもありません。daemon の行が「前のビルド」ではなく「このビルド
ではない」と言うのは、新しいバイナリから手で起動した daemon もここに入るから
です。どちらでも対処は同じです。

Skill の行について 2 点。対象は **いまいるリポジトリ** です。リポジトリの一覧を
持っているわけではないので、別のチェックアウトに古い写しが残っていても、この行
は面倒を見ません。また `--force` を使うため、手で編集した `SKILL.md` も古い写し
と同様に置き換わります。どちらにせよ差分は `git diff` に出ます。

表示は **ビルドごとに 1 回**、`--json` ではない最初の実行時です。これは
`kobune update` が関与しない更新、つまり `install.sh` の再実行、パッケージ
マネージャ、自分でのビルドも同じようにカバーします。状態として持つのは
`~/.kobune/build.json` にある「最後に動いたコミット」だけで、これを書くのは上の
行を表示したあとです。途中で中断された実行は、次にまた見つけ直します。

`kobune daemon` 自身には通知を付けません。`stop` は依頼を書いた時点で戻るため、
直後に確認すると「いま止めたプロセスがまだ動いている」と報告してしまうからです。
記録もしないので、次のコマンドがまとめて表示します。

CLI 自身の発言と同じく stderr へ出し、`--json` では出しません。エージェントの
ストリームは 1 つのドキュメントのままです。`kobune update --json` は同じ内容を
データとして返します。

```json
{
  "status": "installed",
  "commit": "9f3c1a2…",
  "next": [
    {
      "command": "kobune daemon restart",
      "reason": "the daemon is still the previous build"
    }
  ]
}
```

### 自動チェック

1 日 1 回、コマンドが終わったあとに `nightly` の中身を GitHub へ問い合わせ、
実行中のものと違えば **stderr** に 1 行だけ表示します。

```
a newer build is available (9f3c1a2). Run `kobune update`
```

コマンドの前ではなく後に実行するため、通信が遅くても待っている出力が遅れる
ことはありません。`--json` のときは完全に省略されるので、エージェントが解析
するストリームに混ざることはありません。失敗しても何も言いません。GitHub に
到達できなかったチェックには、伝えるべきことがないからです。

止めるときは環境変数を設定します。

```console
$ export KOBUNE_NO_UPDATE_CHECK=1
```

結果は `~/.kobune/update-check.json` に 24 時間キャッシュし、その間はキャッシュ
から同じ 1 行を出し続けます。1 日に 1 度しか出ない警告は、たいてい見落とされる
からです。

### `kobune --version`

このフラグでも同じチェックをします。自動チェックと違って毎回問い合わせます。
`--version` は目の前のビルドについての質問であり、最大 1 日古いキャッシュから
答えるのでは別の質問に答えることになるからです。バージョンを先に表示してから
チェックするので、求めた出力が通信を待つことはありません。

```console
$ kobune --version
kobune 0.1.0 (c7282b8)
› a newer build is available (9f3c1a2). Install it with kobune update
```

公開されているビルドと同じなら何も足しません。どのビルドかはバージョンの行が
すでに答えており、訊かれたのはそれだけだからです。`--json` と
`KOBUNE_NO_UPDATE_CHECK` は自動チェックと同じように省略し、GitHub に到達でき
なかったときも同様です。

ソースからビルドしたものについては、どちらとも言いません。バイナリはビルド元の
コミットを記録していますが、それが無ければ比べる相手がなく、正直に答えようが
ありません。「最新です」は推測になり、「古いです」は意図して作ったビルドから
引き剥がしてしまいます。

```console
$ kobune --version
kobune 0.1.0 (9f3c1a2)
```

## コンテナランタイムの選択

### Docker

追加の設定は不要です。Kobune は Docker API を直接利用し、`docker` CLI を
呼び出しません。そのため CLI 自体はインストールされていなくても、API に
到達できれば動作します。Docker Desktop、OrbStack、colima のいずれでも
構いません。

```console
$ kobune doctor
│ …
│ ✓  container runtime  docker 29.4.0
│ …
```

### Apple Container

macOS 26 以降と、サービスの起動が必要です。

```console
$ container system start
```

`kobune.toml` で指定します。

```toml
[runtime]
default = "apple"
```

選ぶ前に把握しておくべき違いが 3 点あります。
[ランタイム](./runtimes) を参照してください。

## daemon の起動

```console
$ kobune daemon start
╭ kobuned ───────────────────────────────╮
│ running                                │
│                                        │
│ version   0.1.0                        │
│ protocol  1                            │
│ runtime   docker 29.4.0                │
│ uptime    0s                           │
│ socket    ~/.kobune/kobuned.sock       │
╰────────────────────────────────────────╯
```

通常は手動で実行する必要はありません。いずれのコマンドも、daemon が停止して
いれば自動的に起動します。プロキシ、DNS、アイドル判定を担当するため、
常駐プロセスとして動作します。

## 管理者権限が必要な設定

`https://web.myapp.localhost` にポート番号なしでアクセスするには、3 つの設定に
管理者権限が必要です。設定は初回の 1 度だけです。

```console
$ kobune setup
╭ setup ─────────────────────────────────────────────────────────────────╮
│ the URLs need 3 steps, and they need root.                             │
│ each one is shown before it is run, and nothing runs until you say so. │
│                                                                        │
│ 1. let launchd hold 80/443/53 (the daemon itself stays non-root)       │
│ 2. point *.localhost at Kobune's DNS                                   │
│ 3. trust the local CA, so HTTPS stops warning                          │
╰────────────────────────────────────────────────────────────────────────╯

1/3 let launchd hold 80/443/53 (the daemon itself stays non-root)
  generated plist: ~/.kobune/dev.kobune.daemon.plist
  sudo cp ~/.kobune/dev.kobune.daemon.plist /Library/LaunchDaemons/…
  …
run this? [y/N] y
  ✓ done

2/3 point *.localhost at Kobune's DNS
  sudo mkdir -p /etc/resolver && printf 'nameserver 127.0.0.1\n' | sudo tee …
run this? [y/N] n
  – skipped
…
```

**確認なしに実行されるものはありません。** 実行するコマンドは必ず質問より先に
表示されるため、同意する対象は直前に読んだものそのものです。実行しなかった手順
は、最後にまとめて再表示されます。

すべてに同意するなら `kobune setup --yes`、一度も訊かれずにコマンドを読むだけ
なら `kobune setup --dry-run` です。

**応答できる端末がないとき、つまりエージェント・パイプ・`--json` では、
コマンドを表示するだけで何も実行しません。** 自動で `sudo` を実行すると
パスワード入力待ちで停止し、利用者から見れば黙って権限昇格が行われたことに
なります。

実行後は次のようにします。

```console
$ kobune daemon restart   # launchd のジョブとして戻り、標準ポートを確保します
$ kobune doctor
```

### 設定を省略する場合

この設定は必須ではなく、省略するための設定も不要です。80/443 を確保できない
場合、プロキシは代わりに 18080/18443 を使用し、URL にポート番号が付きます。

```console
$ kobune url web
https://web.feat-1.myapp.localhost:18443
```

この状態であることは `kobune doctor` が明示します。

ポートを自分で決める場合は明示的に指定してください。**明示したポートはその
まま使われ、フォールバックしません。**

```console
$ export KOBUNE_HTTP_PORT=8080 KOBUNE_HTTPS_PORT=8443 KOBUNE_DNS_PORT=15353
$ kobune daemon start
```

**DNS にフォールバックはありません。** `/etc/resolver` にポート番号を書く
必要があり、その書き込みにはいずれにせよ root 権限が要るため、DNS だけを
動かしても意味がないためです。これは Kobune ではなく macOS 側の仕様です。
`kobune doctor` が、指定したポートを含んだコマンドをそのまま出力します。

::: tip `kobune setup` を実行済みの場合
プロキシはフォールバック**しません**。launchd はジョブが起動しているか
どうかに関わらず 80 を保持し続けるため、ここでのバインド失敗は「ジョブを
起こす必要がある」という意味になります。別のポートで待ち受けてしまうと、
その状態が隠れてしまいます。どちらであるかは `kobune doctor` が示します。
:::

## 動作確認

```console
$ kobune doctor
```

`✓` 以外の行には、必ず対処方法が併記されます。ここに問題が残ったまま先に
進まないでください。後から発生する原因の分かりにくい不具合は、多くの場合
ここに起因します。

## ファイルの配置場所

`KOBUNE_HOME`（既定値 `~/.kobune`）に、daemon のソケット、状態ファイル、
ログ、ローカル CA、生成されたトンネル設定が保存されます。

Unix ソケットのパスは約 100 バイトまでという制限があるため、`KOBUNE_HOME` に
深い階層のディレクトリは指定できません。Kobune は起動時にこれを検証し、
原因の分かりにくいエラーで失敗する代わりに、その旨を明示します。

## アンインストール

```console
$ kobune uninstall
```

見つかったもの、つまりコンテナ・daemon の状態・バイナリ・補完・root が必要な
手順を一覧で示し、削除する前に確認します。`--dry-run` は一覧を出して終了し、
`--yes` は確認を省略します。端末がない環境では `--yes` が必須です。

**worktree はそのまま残します。** 何が残るか分かるように一覧には表示します。
削除するなら `kobune rm` です。

削除対象の詳細と、root が必要な手順の扱いは
[CLI リファレンス](../reference/cli#アンインストール)にあります。
