# ランタイム

Minato はプロジェクトごとに選んだバックエンドでコンテナを動かします。

```toml
[runtime]
default = "docker"   # または "apple"
```

`minato doctor` は、プロジェクトが使うものと、他に到達できるものを報告します。

```console
$ minato doctor
  ✓ container runtime             apple 1.2.1
  ✓ Docker (available)            docker 29.4.0
```

## Docker

既定で、サポートも厚いほうです。Minato は `bollard` で Docker API を直接叩き、
`docker` CLI を呼ばないので、API に届きさえすれば構いません。Docker Desktop /
OrbStack / colima のどれでも動きます。

ポートは `127.0.0.1` の動的に選ばれたポートにフォワードされます。`0.0.0.0` に
することはありません。同じネットワークの他人から開発環境が見えてしまうから
です。

サービス名はネットワークエイリアスで解決されるので、同じ workspace のどの
コンテナからでも `db:5432` が使えます。

## Apple Container

**macOS 26 以降**と、サービスの起動が必要です。

```console
$ container system start
```

各コンテナが `192.168.x.x` の自分の IP を持つので、ホストに何も publish されず、
ポート衝突も起きません。プロキシはコンテナに直接転送します。

設計上、避けて通れない違いが 2 つあります。

### コンテナ間の名前解決がない

Apple Container にはエイリアスもコンテナ間 DNS もありません。コンテナの
ネームサーバはネットワークのゲートウェイで、どのコンテナ名にも NXDOMAIN を
返します。`db:5432` は動きません。

代わりに Minato が peer の **IP アドレス** を注入します。

```
MINATO_HOST_DB = 192.168.64.7
```

なのでこう書きます。

```js
const db = process.env.MINATO_HOST_DB ?? 'db'
```

::: warning ここでは depends_on が効いてきます
アドレスはサービス起動時に読むので、まだ動いていない peer の変数は
作られません。**`depends_on` を宣言してください。** Minato が正しい順で
起動します。

変数が無いのは意図的です。解決しないホスト名を渡すと、存在しない DNS の
問題を探すことになります。変数が無ければ、順序の問題だと分かります。
:::

### すべてが 1 つのネットワークを共有する

ここではコンテナは 1 つのネットワークにしか参加できず、`network connect` も
ありません。workspace ごとのネットワークにすると、`scope = "project"` の
サービスは最初に起動した worktree に紐づき、他のすべてから届かなくなります
—— その scope が防ぐためにある、まさにそれが起きます。

そのため、すべてのコンテナが既定のネットワークに乗ります。worktree 同士は
ネットワークレベルでは分離されません。1 人が 1 台で行うローカル開発としては
妥当な取引ですが、分離を当てにしていたなら知っておく価値があります。

### その他

- **名前付きボリュームがありません。** Minato は
  `~/.minato/volumes/<project>/` の bind mount に写像し、同等の永続性を
  得ます。
- `minato doctor` は、このランタイムのときは「Docker Desktop を起動」ではなく
  `container system start` と言います。

## どちらを選ぶか

理由がなければ Docker です。ネットワークエイリアス、本物の名前付き
ボリューム、worktree ごとの分離があります。

Apple Container が向くのは、Docker Desktop を動かさずコンテナごとに軽量な VM
が欲しく、サービス間の通信を `MINATO_HOST_*` で書けて、worktree 同士の分離が
要らない場合です。

## Firecracker

未実装です。KVM が必要で macOS ではまったく動かないため、ここで開発する場所が
ありません。`Runtime` trait はまさにこの種の差を吸収するためにあり、Apple
Container の作業でそれが機能することは確認できました —— バックエンドは
プロキシが転送すべきアドレスを返し、プロキシはどれが返したのかを知りません。

## 切り替える

書き換えて起動し直します。

```console
$ minato down --all
$ # minato.toml を編集
$ minato up
```

コンテナは移行しません。古いランタイムのコンテナは消すまで残り、Minato は
自分のラベルが付いたものだけを管理します。
