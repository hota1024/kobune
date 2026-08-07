# Minato

AI エージェント向けの開発環境管理ツール。

**git worktree を作ったら、すぐにプレビュー環境が立ち上がる。**

```console
$ minato new feature/user-auth
✓ worktree created  ~/ghq/github.com/hota1024/myapp.wt/feature-user-auth
✓ web   https://web.feature-user-auth.myapp.localhost
✓ api   https://api.feature-user-auth.myapp.localhost
```

## 特徴

- **worktree = 環境** — worktree が生まれたら環境が生まれ、消えたら消える
- **ポートを覚えない** — サービスごとに `{service}.{workspace}.{project}.localhost` の URL が生える
- **scale-to-zero** — 触っていない環境は自動で停止し、アクセスが来たら起動する。worktree を何個作ってもいい
- **リモートからも見える** — Cloudflare Tunnel でスマホや外部レビュアーに共有できる
- **エージェントが使える** — 全コマンドが `--json` を持ち、Skills から操作できる
- **仮想化を選べる** — Docker / Apple Container / Firecracker を Runtime 抽象で切り替える

## 状態

**M0 〜 M3 完了。** worktree を作るとコンテナが起動し、
`*.localhost` の URL でアクセスできる。触っていない環境は自動で停止し、
アクセスが来たら起き上がる。各サービスには他サービスの URL が
`MINATO_URL_<SERVICE>` として渡る。

```console
$ minato init
$ minato new feature/user-auth
  ✓ worktree feature/user-auth を作成
  ✓ ネットワークを用意
  ✓ イメージ busybox:latest を取得
  ✓ web を起動
  ✓ web の応答を待機

myapp / feature-user-auth  (feature/user-auth)
  /path/to/myapp.wt/feature-user-auth

  web   ready     https://web.feature-user-auth.myapp.localhost
```

標準ポート（80/443）を使うには一度だけ権限の要る設定がいる。
`minato doctor` が状態を診断し、`minato setup` が必要なコマンドを示す
（**sudo は自動実行しない** — 内容を確認してから自分で実行する）。

```console
$ minato setup
1. launchd に 80/443/53 を確保させる（daemon 自体は非 root のまま動きます）
2. *.localhost を Minato の DNS に向ける
3. ローカル CA を信頼する（HTTPS の警告を消す）
```

設定せずに使うなら、非標準ポートを指定すれば権限は要らない:

```console
$ MINATO_HTTP_PORT=8080 MINATO_HTTPS_PORT=8443 MINATO_DNS_PORT=15353 minato daemon start
```

- 設計ドキュメント: [docs/DESIGN.md](docs/DESIGN.md)

## ロードマップ

| | 内容 |
| --- | --- |
| M0 ✅ | Docker / Apple Container runtime + `new` / `up` / `down` / `rm` / `ls` / `status` / `url` |
| M1 ✅ | DNS + リバースプロキシ + TLS + `doctor` / `setup` + launchd socket activation |
| M2 ✅ | scale-to-zero（health check・アイドル停止・オンデマンド起動） |
| M3 ✅ | 環境変数管理（3 層マージ・シークレット参照・URL 自動注入） |
| M4 | Cloudflare Tunnel |
| M5 | Skills |
| M6 | GUI（egui + メニューバー常駐） |
| M7 | Runtime 追加（Apple Container / Firecracker） |

## 構成

Cargo workspace の monorepo。

```
crates/    ライブラリ  core / api / client / runtime / proxy / dns / tunnel
apps/      バイナリ    daemon (minatod) / cli (minato) / desktop (GUI)
skills/    エージェント向け Skill
xtask/     ビルドタスク
```

## 開発

```console
$ cargo build --workspace
$ cargo test --workspace
```

動作させるにはコンテナランタイムが要る。Docker API に到達できれば
`docker` CLI 自体は不要（OrbStack / Docker Desktop / colima のいずれでもよい）。

```console
$ export PATH="$PWD/target/debug:$PATH"
$ minato daemon status
$ minato doctor
```

`MINATO_HOME`（既定 `~/.minato`）に daemon の socket・状態・ログ・CA が置かれる。
Unix socket のパス長制限があるため、深い場所は指定できない。

待ち受けポートは `MINATO_HTTP_PORT` / `MINATO_HTTPS_PORT` / `MINATO_DNS_PORT` で変えられる。
