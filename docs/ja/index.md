---
layout: home

hero:
  name: Minato
  text: git worktree ごとのプレビュー環境
  tagline: ブランチを切れば、環境はすでに動いています。人からもエージェントからも同じように扱えます。
  image:
    src: /logo/minato-mark.svg
    alt: Minato
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
    details: worktree を作れば環境ができ、削除すれば環境も消えます。この対応関係さえ把握していれば、ほかに覚えることはありません。
  - title: ポート番号を管理しない
    details: サービスごとに web.feature-auth.myapp.localhost のような URL が割り当てられ、再起動しても変わりません。内部のポート番号を意識する必要はありません。
  - title: 使っていない環境は自動で停止
    details: 一定時間アクセスのない環境は自動的に停止し、次のリクエストで 1〜2 秒で復帰します。worktree をいくつ作っても負荷は増えません。
  - title: エージェントから操作できる
    details: すべてのコマンドが --json に対応し、終了コードで失敗の種類を判別できます。minato skill install を実行すれば、エージェント向けの操作指針も配置されます。
  - title: ランタイムを選択できる
    details: Docker と Apple Container を共通のインタフェースで扱えます。切り替えは minato.toml の 1 行だけです。
  - title: プレビューを共有できる
    details: Cloudflare Tunnel を経由して、スマートフォンやレビュアーに URL を共有できます。共有先からのアクセスでも自動起動が機能します。
---
