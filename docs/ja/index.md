---
layout: home

hero:
  name: Minato
  text: git worktree ごとのプレビュー環境
  tagline: ブランチを切れば、環境はもう動いている。AI エージェントが操作することを前提に作られています。
  actions:
    - theme: brand
      text: はじめる
      link: /ja/guide/getting-started
    - theme: alt
      text: Minato とは
      link: /ja/guide/
    - theme: alt
      text: GitHub
      link: https://github.com/hota1024/minato

features:
  - title: worktree 1 つに環境 1 つ
    details: worktree が生まれれば環境が生まれ、消えれば消える。この対応関係だけがモデルのすべてで、覚えておくべきこともこれだけです。
  - title: ポート番号を覚えない
    details: サービスごとに web.feature-auth.myapp.localhost のような URL が生え、再起動しても変わりません。裏でポートは変わりますが、URL は変わりません。
  - title: 使っていない環境は止まる
    details: 触っていない環境は自動で停止し、次のリクエストで 1〜2 秒で起き上がります。worktree はいくつ作っても構いません。
  - title: エージェントが操作できる
    details: すべてのコマンドが --json に対応し、終了コードで失敗の種類が分かります。minato skill install で残りはエージェントが学びます。
  - title: ランタイムを選べる
    details: Docker と Apple Container を 1 つの抽象の下に。minato.toml の 1 行で切り替えられます。
  - title: プレビューを共有する
    details: ブランチを Cloudflare Tunnel の後ろに置けば、スマホやレビュアーにリンクを送れます。scale-to-zero もそのまま効きます。
---
