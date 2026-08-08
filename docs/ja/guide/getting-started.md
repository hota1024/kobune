# 最初の環境

空のリポジトリから、URL でアクセスできる状態までを一通り行います。所要時間は
10 分程度で、その大半はイメージの取得待ちです。

[インストール](./installation) を済ませ、`minato doctor` が問題なく通ることを
前提とします。

## 1. プロジェクトを定義する

git リポジトリのルートで実行します。

```console
$ minato init
created /path/to/myapp/minato.toml
project: myapp

next, bring the environment up with `minato up`
```

`minato init` はひな形を生成し、ディレクトリ名からプロジェクト名を推測します。
生成されたファイルを開き、実際の構成に合わせて編集します。

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

ここで重要なのは次の 3 点です。

- **`port`** はアプリケーションが**コンテナ内で**待ち受けるポートです。ホスト
  側のポートを指定する項目はなく、意識する必要もありません。
- **`command`** はイメージ側のコマンドを上書きします。省略した場合はイメージの
  既定値が使われます。
- worktree は **`/workspace`** にマウントされ、そこが作業ディレクトリになり
  ます。したがって `npm run dev` は、そのブランチのコードに対して実行されます。

編集したらコミットしてください。`minato.toml` はリポジトリで管理するファイル
であり、すべての worktree が同じ内容を参照します。

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

main worktree は URL から workspace 名を省略するため、
`web.main.myapp.localhost` ではなく `web.myapp.localhost` になります。

最後の `waiting for web` は、コンテナの起動ではなくアプリケーションが実際に
応答するまでを待っています。この 2 つは別の状態であり、コンテナ起動直後の
`curl` は多くの場合失敗します。

## 3. アクセスする

```console
$ curl -sS --fail-with-body https://web.myapp.localhost
```

URL を取得してから使うこともできます。

```console
$ minato url web
https://web.myapp.localhost
```

`minato url` は 1 行だけを出力するため、パイプやコマンド置換にそのまま使え
ます。URL を直接記述するのではなく、このコマンドで取得してください。

::: tip 証明書エラーが出る場合
`curl` が終了コード 60 で失敗するのは、ローカル CA がまだ信頼されていないため
です。`minato doctor` が対処方法を出力します。最初に遭遇する問題としては、
これが最も多いものです。
:::

## 4. ブランチを作り、2 つ目の環境を得る

worktree を使う利点が現れるのはここからです。

```console
$ minato new feature/user-auth
  ✓ creating worktree feature/user-auth
  ✓ starting web
  ✓ waiting for web

myapp / feature-user-auth  (feature/user-auth)
  /path/to/myapp.wt/feature-user-auth

  web   ready     https://web.feature-user-auth.myapp.localhost
```

2 つの環境が、別々の URL と別々のチェックアウトで動作しています。既存の環境は
停止しておらず、ポート番号の指定も不要です。

worktree は `../myapp.wt/feature-user-auth` に作成されます。リポジトリの内側
ではなく隣接するディレクトリに置くため、エディタや検索の対象が二重になりません。

```console
$ minato ls
WORKSPACE            SERVICES    BRANCH
(main)               1/1         main
feature-user-auth    1/1         feature/user-auth
```

## 5. worktree 内で作業する

```console
$ cd ../myapp.wt/feature-user-auth
```

worktree の内側では、コマンドは既定でその workspace を対象とします。

```console
$ minato logs web -f          # このブランチのログを追跡する
$ minato exec web -- npm test # このコンテナ内でテストを実行する
$ minato status               # 起動状況とアクセス先を確認する
```

別のディレクトリからは `-w` で対象を指定します。

```console
$ minato logs -w feature-user-auth web
```

## 6. 放置した場合の挙動

一定時間アクセスがなければ、環境は自動的に停止します。再びアクセスすると、
最初のリクエストで起動します。

```console
$ curl -sS https://web.feature-user-auth.myapp.localhost
# 1〜2 秒待って応答が返る
```

作業中のブランチに対して `minato up` を再実行する必要はありません。放置した
環境がリソースを消費しないため、worktree を必要なだけ作成できます。

## 7. 後片付け

```console
$ minato rm -w feature-user-auth
```

worktree とコンテナを削除します。ブランチは残ります。このコマンドは
`git branch -d` とは異なります。

## 次に読むもの

- [設定](./configuration) — 複数サービス、ヘルスチェック、ボリューム
- [基本操作](./workflow) — 実際によく使うコマンド
- [ブランチごとのプレビュー](../tutorials/first-preview) — 同じ内容を実際の
  アプリケーションで一通り実施する
