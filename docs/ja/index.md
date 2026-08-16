---
layout: home

hero:
  name: Kobune
  text: git worktree ごとのプレビュー環境
  tagline: ブランチを切れば、環境は専用の URL ですでに動いています。人からもエージェントからも同じように扱えます。
  actions:
    - theme: brand
      text: はじめる
      link: /ja/guide/getting-started
    - theme: alt
      text: Kobune とは
      link: /ja/guide/
    - theme: alt
      text: GitHub
      link: https://github.com/hota1024/kobune
  install:
    command: "curl -fsSL https://minato.1024.works/install.sh | sh"
    copy: コピー
    copied: コピーしました

specs:
  title: できること
  items:
    - label: worktree 1 つに環境 1 つ
      body: worktree を作れば環境ができ、削除すれば環境も消えます。覚えることはこの対応関係だけです。
    - label: ポート番号を管理しない
      body: サービスごとに web.feature-auth.myapp.localhost のような URL が割り当てられます。内部のポート番号が変わっても、URL は変わりません。
    - label: 使っていない環境は自動で停止
      body: 一定時間アクセスのない環境は自動的に停止し、次のリクエストで 1〜2 秒で復帰します。
    - label: エージェントから操作できる
      body: すべてのコマンドが --json に対応し、終了コードで失敗の種類が分かります。
    - label: Docker と Apple Container
      body: 2 つのランタイムを共通のインタフェースで扱えます。切り替えは kobune.toml の 1 行だけです。
    - label: トンネルで共有できる
      body: Cloudflare Tunnel を経由して、スマートフォンやレビュアーに URL を共有できます。

notes:
  title: Kobune の対象外
  items:
    - lead: 本番環境向けのデプロイツールではありません。
      body: 開発マシン上で、その所有者が操作することを前提としています。
    - lead: コンテナランタイムではありません。
      body: 実際にコンテナを動かすのは Docker や Apple Container です。Kobune はその手配をするだけです。
    - lead: Docker Compose の代替でもありません。
      body: 1 つのブランチで 1 つの環境を動かせば足りるのであれば、Compose のほうが単純です。
  link: /ja/guide/how-it-works
  linkText: 仕組み
---
