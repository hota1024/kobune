---
name: minato
description: git worktree ごとのプレビュー環境を操作する。ブランチを切って動作確認する、サービスのログを見る、コンテナ内でテストを走らせる、環境変数を設定するときに使う。minato.toml があるリポジトリで有効。
---

# Minato

worktree 1 つにつき環境 1 つが対応している。worktree が生まれれば環境が生まれ、
消えれば消える。この対応さえ押さえておけば、自分がどの環境を見ているのかを
見失わない。

## 原則

**`docker` を直接使わない。** `docker ps` や `docker logs` で見えるものは
Minato 経由でも見える。直接触ると、Minato が把握している状態と食い違う。

**ポート番号を推測しない。** `minato url` で取る。ポートは起動のたびに
変わることがあり、URL は変わらない。

**確認は必ず実際のアクセスで行う。** 「起動したはず」で終わらせない。

## 最初にすること

```bash
minato status --json
```

これで現在の workspace、各サービスの状態、URL が分かる。
`minato.toml` が無ければ `minato init` で雛形を作る。

## よく使う操作

### 新しいブランチで作業を始める

```bash
minato new feature/user-auth
```

worktree の作成と環境の起動をまとめて行い、URL まで出す。
`git worktree add` を自分で叩く必要はない（叩いた場合も Minato は認識する）。

作った worktree に移動してから作業する。パスは `minato status --json` の
`path` に出る。

### 変更を確認する

```bash
URL=$(minato url web)
curl -sS --fail-with-body "$URL/api/health"
```

**`curl -s` だけで済ませない。** エラーが握り潰されて「空の応答が返った」
ようにしか見えなくなる。`-sS --fail-with-body` を付けるか、終了コードを見る。

`minato url` は `https://web.feature-user-auth.myapp.localhost` のような
URL を 1 行で返す。**この URL は停止中でも有効**で、アクセスすると環境が
起き上がる。数秒待たされることがあるが、`curl` は準備ができるまで待つので
そのまま使ってよい。

### ログを見る

```bash
minato logs                 # 全サービス
minato logs web -n 50       # web の直近 50 行
minato logs web -f          # 流し続ける（自分で止めること）
```

### コンテナ内でコマンドを実行する

```bash
minato exec web -- pnpm test
```

**終了コードはコマンドのものがそのまま返る。** テストの成否は終了コードで
判定できる。出力は stdout / stderr に分かれて届く。

### 環境変数

```bash
minato env ls               # どの層の値が効いているかも分かる
minato env set API_KEY=xxx  # 既定では workspace 層（この worktree だけ）
minato env set DEBUG=1 --scope project   # リポジトリ全体
```

`.env` を直接書かない。層が 3 つあり、どれが効いているか分からなくなる。

**設定を変えたら `minato down && minato up` が要る。** 起動中のコンテナには
反映されない。

各サービスには他サービスの URL が `MINATO_URL_<SERVICE>` として渡っている
（`api` サービスなら `MINATO_URL_API`）。フロントから API を呼ぶときは
これを使う。ハードコードすると worktree ごとに壊れる。

### 片付ける

```bash
minato rm -w feature-user-auth
```

worktree と環境を消す。ブランチは残る。

## うまくいかないとき

**手順は必ずこの順で。** 推測で `docker` に戻らない。

1. `minato status --json` — サービスの `state` を見る
   - `stopped` → アクセスするか `minato up` で起動する
   - `starting` → 待つ
   - `failed` → `reason` に理由がある
2. `minato logs <service>` — アプリ側のエラーを見る
3. `minato doctor` — 環境側の問題を見る。**直し方が `fix` に出る**

### よくある症状

| 症状 | 見るところ |
| --- | --- |
| `curl` が終了コード 60 | 証明書が信頼されていない。`minato doctor` → CA を信頼させる手順が出る（sudo が要るので人に頼む） |
| URL に繋がらない | `minato doctor`。DNS や プロキシの設定が未完のことが多い |
| 404 が返る | ホスト名が違う。`minato url` で取り直す |
| 502 が返る | サービスは登録されているが応答していない。`minato logs` |
| 起動が終わらない | `minato logs -f` で進行を見る |
| 設定を変えたのに効かない | `minato down && minato up` |

## 出力の扱い

すべてのコマンドが `--json` に対応している。解析するときは付ける。

失敗時は終了コードで種類が分かる。

| コード | 意味 |
| --- | --- |
| 4 | 見つからない（workspace / サービス） |
| 5 | 既に存在する |
| 6 / 7 | 設定が無い / 不正 |
| 8 | git リポジトリの外 |
| 9 | コンテナランタイムに繋がらない |
| 10 | ランタイムの操作が失敗した |
| 11 | 未対応の機能 |

`--json` のエラーには `hint` が入っていることがある。**次に何をすべきかが
書かれているので読む。**

## してはいけないこと

- `docker` / `container` コマンドを直接叩く
- ポート番号を URL に埋め込む（`localhost:3000` など）
- `.env` を直接編集する
- `minato logs -f` を止めずに放置する
- 動作確認をせずに「起動した」と報告する
- `curl -s` の空の出力を「応答が空」と解釈する（証明書エラーの可能性がある）
