---
layout: home

hero:
  text: git worktree ごとのプレビュー環境
  tagline: ブランチを切れば、環境は専用の URL ですでに動いています。人からもエージェントからも同じように扱えます。
  actions:
    - theme: brand
      text: はじめる
      link: /ja/guide/getting-started
    - theme: alt
      text: Kobune とは
      link: /ja/guide/
  install:
    command: "curl -fsSL https://kobune.1024.works/install.sh | sh"
    copy: コピー
    copied: コピーしました

config:
  note: このファイルを 1 つコミットしておけば、あとから増える worktree すべてがこれを読みます。

steps:
  title: 何が起きるか
  items:
    - command: kobune init
      body: 上のファイルを書き出します。リポジトリのほかの部分は変わりません。
    - command: kobune new feature/user-auth
      body: worktree を作り、その環境も一緒に立ち上げます。
    - command: https://web.feature-user-auth.myapp.localhost
      url: true
      body: あとは開くだけです。ポート番号は誰も選んでおらず、再起動しても同じ名前で届きます。

compare:
  title: docker compose から移ってくる場合
  body: サービスの内容はいま書いているものと同じです。変わるのは、チェックアウト単位ではなく worktree 単位で環境ができることと、サービスごとにブランチではなくプロジェクトに属すると宣言できることです。
  note: kobune init --from-compose を実行すると、左のファイルから右のファイルを書き出します。最初の kobune up の前に、残された TODO を読んでください。docker compose には Kobune に対応するキーがない設定もあり、その部分は推測せずに印を付けます。

specs:
  title: できること
  items:
    - label: worktree 1 つに環境 1 つ
      body: worktree を作れば環境ができ、削除すれば環境も消えます。覚えることはこの対応関係だけです。
    - label: ポート番号を管理しない
      body: 内部のポート番号は起動のたびに変わりますが、URL は変わりません。
    - label: 使っていない環境は自動で停止
      body: 一定時間アクセスのない環境は自動的に停止し、次のリクエストで 1〜2 秒で復帰します。
    - label: 名前が変わらない
      body: web.feature-auth.myapp.localhost はブランチ名から作られるので、明日も同じ名前です。
    - label: 共有すべきものは共有する
      body: データベースはブランチではなくプロジェクトに属するものとして、全体で 1 つだけ起動できます。
    - label: トンネルで共有できる
      body: Cloudflare Tunnel を経由して、スマートフォンやレビュアーに URL を共有できます。

agents:
  title: エージェントも同じコマンドを使う
  body: 人が使うのと同じコマンドで、プログラムが読む部分だけ --json を付けます。失敗したときは種類の分かる終了コードが返るので、エージェントは空の応答ではなく理由を受け取ります。
  link: /ja/guide/agents
  linkText: AI エージェントと使う

runtimes:
  title: コンテナを動かすもの
  lead: kobune.toml の [runtime] default に、次のいずれかを書きます。コンテナの手配をするのが Kobune で、実際に起動するのはランタイムです。
  items:
    - key: docker
      state: 既定で、2 つのうち対応が手厚いほうです。
      name: docker コマンドではなく Docker API を直接呼ぶため、Docker Desktop でも OrbStack でも colima でも動きます。
      ready: true
    - key: apple
      state: 利用できます。macOS 26 以降で、container system start を実行済みであることが必要です。
      name: コンテナごとに独自のアドレスが割り当てられ、ホスト側には何も公開されません。worktree が 2 つあってもポートは衝突しません。
      ready: true
    - key: firecracker
      state: 対応予定です。
      ready: false

notes:
  title: Kobune がやらないこと
  items:
    - lead: 本番環境向けのデプロイツールではありません。
      body: 開発マシン上で、その所有者が操作することを前提としています。
    - lead: コンテナランタイムではありません。
      body: 実際にコンテナを動かすのは Docker や Apple Container です。Kobune はその手配をするだけです。
    - lead: docker compose の代替でもありません。
      body: 1 つのブランチで 1 つの環境を動かせば足りるのであれば、docker compose のほうが単純です。
  link: /ja/guide/how-it-works
  linkText: 仕組み
---
