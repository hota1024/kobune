# AI エージェントと使う

Minato はエージェントによる操作を前提に設計しています。それが具体的に何を
意味するか、また必要な設定について説明します。

## Skill を配置する

```console
$ minato skill install
installed /path/to/myapp/.claude/skills/minato/SKILL.md
```

Claude Code が自動的に読み込む Skill ファイルを生成します。コミットしておけば、
すべての worktree とチームメンバーが同じ指針を参照できます。

```console
$ minato skill show              # 書き込まずに内容を表示する
$ minato skill install --force   # 手動で編集した内容を上書きする
```

内容に変更がなければ書き込みを行わないため、差分は発生しません。

## Skill に記述されている内容

コマンドのリファレンスではありません。それは `--help` の役割です。記述して
いるのは、エージェントが自力では導けない判断基準です。

- **`docker` を直接使用しない。** `docker ps` で確認できる情報は Minato 経由
  でも取得できます。直接操作すると、実際の状態と Minato が把握している状態が
  食い違います。
- **ポート番号を推測しない。** `minato url` で取得します。ポート番号は変わり
  ますが、URL は変わりません。
- **確認は必ず実際のアクセスで行う。** 「起動したはず」では確認したことに
  なりません。
- **`curl -s` だけでは不十分。** エラーが握り潰されるため、信頼されていない
  証明書によるエラーと空の応答を区別できません。
- **トンネルを有効化しない。** インターネットへの公開は利用者が判断すべき
  事項です。

## この設計にした理由

次の 3 つの決定は、出力を読むのが人間ではなくエージェントであることに
由来します。

### すべてのコマンドが JSON を出力する

```console
$ minato status --json
{
  "result": "workspace",
  "workspace": {
    "project": "myapp",
    "services": [
      { "name": "web", "state": { "state": "ready" },
        "url": "https://web.feature-auth.myapp.localhost" }
    ]
  }
}
```

人間向けに整形されたテキストから情報を抽出する必要がありません。

### 終了コードで失敗の種類が分かる

```console
$ minato url nope; echo $?
4
```

| コード | 意味 |
| --- | --- |
| 4 | 見つからない |
| 5 | すでに存在する |
| 6 / 7 | 設定が存在しない / 不正 |
| 8 | git リポジトリの外 |
| 9 | コンテナランタイムに接続できない |
| 10 | ランタイムの操作が失敗した |
| 11 | 未対応 |

エージェントは出力を解析せずに分岐できます。全一覧は
[終了コードのリファレンス](../reference/exit-codes) を参照してください。

### `exec` は終了コードをそのまま返す

```console
$ minato exec web -- npm test; echo $?
1
```

テストの成否を終了コードだけで判定できるようにするための仕様です。

### エラーには hint が付与される

```console
$ minato tunnel enable --domain example.com --json
{
  "error": {
    "code": "unsupported",
    "message": "a tunnel exposes this environment to the internet",
    "hint": "put a Cloudflare Access policy in front of the hostname, then re-run with --public"
  }
}
```

`hint` には、何が起きたかではなく次に取るべき操作を記述しています。

## エラーを返さず待機する

停止中の環境が起動するまでには数秒かかります。その間の挙動は、リクエストの
送信元によって変わります。

- **ブラウザ**には起動中であることを示すページを返し、自動的にリロードさせ
  ます。
- **それ以外**（curl、fetch、エージェント）は、応答可能になるまで最大 120 秒
  待機させます。

後者は意図的な設計です。起動中に 503 を返すと、エージェントはサーバが故障して
いると判断し、問題のないコードを修正しようとします。

## 実用的な処理の流れ

```bash
minato status --json                       # 現在の状態を把握する
minato new feature/x                       # ブランチと環境をまとめて作成
cd ../myapp.wt/feature-x
# … 編集 …
minato exec web -- npm test                # 終了コードがテスト結果になる
curl -sS --fail-with-body "$(minato url web)/api/health"
minato logs web -n 50                      # 失敗した場合
minato doctor                              # 環境側が原因の場合
```

## MCP サーバを提供していない理由

意図的に提供していません。すべてのコマンドが `--json` に対応している以上、
Bash 経由で十分に扱えます。インタフェースを二重に維持するコストに見合いません。
