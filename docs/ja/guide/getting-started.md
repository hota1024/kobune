# 最初の環境

空のリポジトリから動く URL まで。10 分ほど、その大半はイメージの取得待ちです。

[インストール](./installation) を済ませ、`minato doctor` が通っている前提です。

## 1. プロジェクトを書く

git リポジトリのルートで:

```console
$ minato init
created /path/to/myapp/minato.toml
project: myapp

next, bring the environment up with `minato up`
```

`minato init` はひな形を書き、ディレクトリ名からプロジェクト名を推測します。
開いて、実際のものに向けます。

```toml
[project]
name = "myapp"

[runtime]
default = "docker"

[services.web]
image = "node:22"
port = 3000
command = "npm run dev"
```

大事なのは 3 つです。

- **`port`** は、アプリが**コンテナの中で**待ち受けるポートです。ホスト側の
  ポートを聞かれることはありませんし、知る必要もありません。
- **`command`** はイメージのコマンドを置き換えます。省略すればイメージの
  既定が使われます。
- worktree は **`/workspace`** にマウントされ、そこが作業ディレクトリになり
  ます。つまり `npm run dev` はそのブランチのコードに対して走ります。

コミットしてください。`minato.toml` はリポジトリに属します。すべての worktree
が同じものを読みます。

## 2. 起動する

```console
$ minato up
  ✓ preparing the network
  ✓ pulling image node:22
  ✓ starting web
  ✓ waiting for web

myapp / (main)  (main)
  /path/to/myapp

  web   ready     https://web.myapp.localhost
```

main worktree は URL から workspace ラベルを省くので、
`web.main.myapp.localhost` ではなく `web.myapp.localhost` になります。

最後の `waiting for web` は、コンテナができるのを待っているのではなく、
アプリが実際に応答するのを待っています。この 2 つは別物で、コンテナ起動直後の
`curl` はたいてい失敗します。

## 3. アクセスする

```console
$ curl -sS --fail-with-body https://web.myapp.localhost
```

または URL を受け取って使います。

```console
$ minato url web
https://web.myapp.localhost
```

`minato url` は 1 行だけを出すので、パイプにもコマンド置換にもそのまま
使えます。URL を手で書く代わりに、こちらを使ってください。

::: tip 証明書エラー
`curl` が終了コード 60 で失敗するのは、ローカル CA がまだ信頼されていない
ためです。`minato doctor` が直し方を出します。最初にぶつかるものとして、
これが一番多いです。
:::

## 4. ブランチを切って、2 つ目の環境を得る

worktree を使う意味が出てくるのはここからです。

```console
$ minato new feature/user-auth
  ✓ creating worktree feature/user-auth
  ✓ starting web
  ✓ waiting for web

myapp / feature-user-auth  (feature/user-auth)
  /path/to/myapp.wt/feature-user-auth

  web   ready     https://web.feature-user-auth.myapp.localhost
```

これで 2 つの環境が、別々の URL・別々のチェックアウトで動いています。何も
止まっておらず、誰もポートを選んでいません。

worktree は `../myapp.wt/feature-user-auth` にできます。リポジトリの中ではなく
隣なので、エディタや検索が二重に拾いません。

```console
$ minato ls
WORKSPACE            SERVICES    BRANCH
(main)               1/1         main
feature-user-auth    1/1         feature/user-auth
```

## 5. その中で作業する

```console
$ cd ../myapp.wt/feature-user-auth
```

worktree の中では、コマンドは既定でその workspace を対象にします。

```console
$ minato logs web -f          # このブランチのログを追う
$ minato exec web -- npm test # そのコンテナの中でテストを走らせる
$ minato status               # 何が動いていて、どこにあるか
```

別の場所からは `-w` で指定します。

```console
$ minato logs -w feature-user-auth web
```

## 6. 放っておく

しばらく触らなければ環境は自分で止まります。戻ってくれば、最初のリクエストが
起こします。

```console
$ curl -sS https://web.feature-user-auth.myapp.localhost
# 1〜2 秒待って、応答
```

まだ使っているブランチに対して `minato up` を打ち直す必要はありません。
worktree を気軽に作れるのは、放置した環境が何も食わないからです。

## 7. 片付ける

```console
$ minato rm -w feature-user-auth
```

worktree とコンテナを消します。ブランチは残ります。これは
`git branch -d` ではありません。

## 次に

- [設定](./configuration) — 複数サービス、ヘルスチェック、ボリューム
- [日々の使い方](./workflow) — 実際に使うコマンド
- [ブランチごとのプレビュー](../tutorials/first-preview) — 同じ内容を実際の
  アプリで通しでやる
