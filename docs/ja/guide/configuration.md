# 設定

すべてはリポジトリルートの `minato.toml` にあります。コミットされ、すべての
worktree が同じものを読みます。

キーの網羅的な一覧は
[`minato.toml` リファレンス](../reference/minato-toml) にあります。このページ
はその背後にある判断についてです。

## 最小の設定

```toml
[project]
name = "myapp"

[services.web]
image = "node:22"
port = 3000
command = "npm run dev"
```

プロジェクト名はすべての URL に現れるので、1 つの daemon が管理する
プロジェクトの中で一意である必要があります。同じ名前の 2 つを登録しようと
すると、URL が衝突するより先に拒否されます。

## 複数のサービス

```toml
[services.web]
image = "node:22"
port = 3000
command = "npm run dev"
depends_on = ["api"]

[services.api]
image = "node:22"
port = 8080
command = "npm run api"
depends_on = ["db"]

[services.db]
image = "postgres:16"
port = 5432
scope = "project"
expose = false
volumes = ["pgdata:/var/lib/postgresql/data"]
env = { POSTGRES_PASSWORD = "postgres" }
```

`depends_on` は起動順を決めます。依存先が *health* になるまで待つわけでは
ありませんが、順番に起動し、それぞれに立ち上がる時間を与えます。

## scope: worktree ごとか、共有か

いちばん重要な選択です。

```toml
scope = "workspace"   # 既定。worktree ごとに 1 インスタンス
scope = "project"     # すべての worktree で 1 インスタンスを共有
```

worktree ごとのデータベースは、それぞれに seed が要り、全部ぶんのリソースを
払うことになります。共有すればどのブランチも同じデータを見ます。開発中は
たいていそれが望みですが、2 つのブランチが別々のマイグレーションを持つときは
まさにそれが困ります。

Minato はこのマイグレーション問題を解決しません。共有データベースに互換性の
ないマイグレーションを当てるブランチが 2 つあれば、ぶつかります。そういう
ものには `scope = "workspace"` を使い、seed のコストを払ってください。

## 公開するかどうか

```toml
expose = false
```

`expose = false` のサービスには URL もルートも作られません。他のサービスから
内部的には届きますが、環境の外からは届きません。データベースやキャッシュは
ほぼ常に設定すべきです。

`port` があるときの既定は true です。

## ヘルスチェック

```toml
health = "http://localhost:3000/healthz"
health = "tcp://localhost:5432"
```

Minato が「起動した」ではなく「受け付けられる」と判断する方法です。これが
ないと、判定は「TCP 接続が通るか」だけになり、HTTP サービスでは応答できる
ようになるずっと前に true になり得ます。

2 つ知っておくべきことがあります。

- `http://` では **パスだけが使われます。** 書くのはコンテナの中から見た
  アドレスで、Minato が届くのはランタイムが割り当てたアドレスです。書いた
  ホストとポートは無視されます。
- **`cmd:` はまだ未対応です。** コンテナの中で実行する必要があり、そこは
  繋がっていません。

scale-to-zero で停止中のサービスをリクエストが起こすときも、この判定を
待ちます。良いヘルスチェックは最初のリクエストを速く確実にします。

## アイドルタイムアウト

```toml
idle_timeout = "30m"
```

リクエストが来ないまま何分で自分を止めるか。既定は 30 分です。起動が遅いもの
には長く、worktree をたくさん作るなら短く。

計るのはプロキシを通った最後のリクエストからです。コンテナ間の通信は
数えないので、他のサービスからしか呼ばれないサービスは、呼び元が動いていても
止まります。そういうものには長めの値か、なしを。

## ボリューム

```toml
volumes = [
  "pgdata:/var/lib/postgresql/data",   # 名前付き。ランタイムが管理
  "./seed:/seed",                      # ホストのパス。worktree からの相対
  "/etc/ssl/certs:/certs:ro",          # 絶対パス、読み取り専用
]
```

スラッシュを含まない名前はランタイムが管理する領域で、プロジェクト単位なので
worktree 間で共有されます。`/` `./` `~/` で始まるものはホストのパスです。

名前付きボリュームを使う典型は Node の `node_modules` です。workspace ごとに
`/workspace/node_modules` に置けば、インストールは一度で済みます。

## 環境変数

小さくて秘密でない値はここに置けます。

```toml
[services.web]
env = { NODE_ENV = "development" }
```

それ以外 —— マシンごと・worktree ごとに違うもの、秘密のもの —— は層になった
環境変数に置いてください。[環境変数](./environment-variables) を参照。

## ランタイムを選ぶ

```toml
[runtime]
default = "docker"   # または "apple"
```

プロジェクト単位なので、リポジトリごとに別のバックエンドを使えます。切り替えで
何が変わるかは [ランタイム](./runtimes) を参照。

## URL の接尾辞

```toml
[project]
name = "myapp"
domain = "myapp.localhost"   # 既定。name から導出される
```

`domain` を上書きすれば別の名前で配れます。ただし何を選んでも 127.0.0.1 に
解決される必要があり、`.localhost` 以外なら `/etc/resolver` の設定が
もう 1 つ要ります。

## まだ対応していないもの

- **`build`** — Dockerfile からのビルド。既製イメージにソースをマウントする
  ほうが起動が速く、「すぐ立ち上がる」という狙いにも合います。予定はあります。
- **`minato.local.toml`** — worktree ごとの上書き。環境変数の層でほとんどの
  用途は埋まっています。
