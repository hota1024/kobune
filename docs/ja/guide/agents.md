# AI エージェントと使う

Minato はエージェントが操作することを前提に作られています。それが実際に
どういうことか、どう設定するかを説明します。

## Skill を配置する

```console
$ minato skill install
installed /path/to/myapp/.claude/skills/minato/SKILL.md
```

Claude Code が自動的に読む Skill ファイルを書き出します。コミットしてください。
すべての worktree とチームメンバーが同じ指示を得ます。

```console
$ minato skill show              # 書かずに表示する
$ minato skill install --force   # 手で書き換えたものを上書きする
```

内容が同じなら書き直さないので、リポジトリを汚しません。

## Skill に書いてあること

コマンドのリファレンスではありません。それは `--help` の役目です。書いてある
のは、エージェントが自分では導けない判断です。

- **`docker` を直接使わない。** `docker ps` で見えるものは Minato 経由でも
  見えます。直接触ると、実際の状態と Minato の把握が食い違います。
- **ポート番号を推測しない。** `minato url` で取ります。ポートは変わり、
  URL は変わりません。
- **確認は必ず実際のアクセスで。** 「起動したはず」は確認ではありません。
- **`curl -s` だけでは足りない。** エラーを握り潰すので、信頼されていない
  証明書は空の応答と見分けが付きません。
- **トンネルを有効化しない。** インターネットに公開するかは利用者が決めます。

## なぜこういう設計なのか

3 つの決定は、人ではなくエージェントが出力を読むことに由来します。

### すべてのコマンドが JSON を話す

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

人向けのテキストから何かを取り出す必要がありません。

### 終了コードが失敗の種類を示す

```console
$ minato url nope; echo $?
4
```

| コード | 意味 |
| --- | --- |
| 4 | 見つからない |
| 5 | 既に存在する |
| 6 / 7 | 設定が無い / 不正 |
| 8 | git リポジトリの外 |
| 9 | コンテナランタイムに繋がらない |
| 10 | ランタイムの操作が失敗した |
| 11 | 未対応 |

エージェントは何も読まずに分岐できます。全一覧は
[終了コードのリファレンス](../reference/exit-codes) にあります。

### `exec` は終了コードをそのまま返す

```console
$ minato exec web -- npm test; echo $?
1
```

テストの成否が終了コードだけで分かる、というのがすべてです。

### エラーには hint が付く

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

`hint` は「何が起きたか」ではなく「次に何をすべきか」を書きます。

## 失敗させず、待たせる

停止中の環境が立ち上がるには数秒かかります。その間どうなるかは、誰が聞いたか
によります。

- **ブラウザ** には「起動中」のページを返し、自動でリロードさせます。
- **それ以外** —— curl、fetch、エージェント —— は受け付けられるまで、最大
  120 秒待たせます。

後者は意図的です。起動中に 503 を返すと、エージェントは「サーバが壊れている」
と受け取り、まったく悪くないコードを直しに行きます。

## 実際に回るループ

```bash
minato status --json                       # いまどうなっているか
minato new feature/x                       # ブランチと環境をまとめて
cd ../myapp.wt/feature-x
# … 編集 …
minato exec web -- npm test                # 終了コードがテスト結果
curl -sS --fail-with-body "$(minato url web)/api/health"
minato logs web -n 50                      # 失敗したら
minato doctor                              # 環境側が原因なら
```

## MCP サーバは作っていません

意図的です。すべてのコマンドが `--json` を持つ以上 Bash で足ります。
もう 1 つの面を正しく保ち続けるコストに見合いません。
