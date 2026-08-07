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

**M0 完了。** worktree を作るとコンテナが起動し、ホストのポート経由でアクセスできる。
URL（`*.localhost`）はまだ生えない — M1 で対応する。

```console
$ minato init
$ minato new feature/user-auth
  ✓ worktree feature/user-auth を作成
  ✓ ネットワークを用意
  ✓ イメージ busybox:latest を取得
  ✓ web を起動

myapp / feature-user-auth  (feature/user-auth)
  /path/to/myapp.wt/feature-user-auth

  web   ready     http://127.0.0.1:32768
```

- 設計ドキュメント: [docs/DESIGN.md](docs/DESIGN.md)

## ロードマップ

| | 内容 |
| --- | --- |
| M0 ✅ | Docker / Apple Container runtime + `new` / `up` / `down` / `rm` / `ls` / `status` / `url` |
| M1 | DNS + リバースプロキシ + TLS |
| M2 | scale-to-zero |
| M3 | 環境変数管理 |
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
```

`MINATO_HOME`（既定 `~/.minato`）に daemon の socket・状態・ログが置かれる。
Unix socket のパス長制限があるため、深い場所は指定できない。
