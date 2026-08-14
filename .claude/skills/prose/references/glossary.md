# Glossary

One rendering per term, in both directions. Counts are from `docs/ja/` at
`aa141bf`; where a minority spelling is listed it is an outlier to be fixed, not
an alternative.

## The two that are confused

**`worktree`** is git's. A checkout of a branch beside the repository, made by
`git worktree add` or by `kobune new`.

**`workspace`** is Kobune's. The environment a worktree has: its containers, its
URLs, its label. `kobune ls` lists workspaces; `-w` names one.

The main worktree has a workspace too, labelled `(main)`, and leaves its label
out of the URLs. Neither word is ever the other, and neither is ever written in
katakana — `ワークツリー` and `ワークスペース` appear 0 times and should stay
that way.

## Terms

| English | 日本語 | Notes |
| --- | --- | --- |
| worktree | `worktree` | Latin, lowercase, both languages |
| workspace | `workspace` | Latin, lowercase, both languages |
| daemon | `daemon` | Latin. `デーモン` appears 3 times against 82 and is an outlier. Always lowercase in English (87/87) |
| scale-to-zero | `scale-to-zero` | Latin, hyphenated. Not `ゼロスケール` |
| service | サービス | 161 |
| container | コンテナ | 101 |
| project | プロジェクト | |
| runtime | ランタイム | 34 |
| image | イメージ | 26 |
| volume | ボリューム | 33 |
| named volume | 名前付きボリューム | 5 |
| bind mount | バインドマウント | 3 |
| to mount | マウントする | 17 |
| health check | ヘルスチェック | 3. The key itself is `health` |
| proxy | プロキシ | 35. No long vowel mark |
| gateway | ゲートウェイ | |
| tunnel | トンネル | 17. `Cloudflare Tunnel` is a name and stays as it is |
| certificate | 証明書 | 11. The authority is `CA` |
| preview | プレビュー | 8 |
| repository | リポジトリ | 28. Not `レポジトリ` |
| branch | ブランチ | 51 |
| host | ホスト | 32 |
| flag | フラグ | 7. Not `オプション` |
| key | キー | 20, for a `kobune.toml` key |
| exit code | 終了コード | 22 |
| environment variable | 環境変数 | 26 |

## State names

`ready`, `starting`, `stopped` and `failed` are the strings the program prints
and `--json` carries. They stay in Latin in both languages, in code spans, and
are never translated — `準備完了` for `ready` would be a state that does not
exist.

Japanese describes around them: 「`ready` になるまで待ちます」.

## Names, spelled as their owners spell them

`Kobune` (the project) · `kobune` (the command, always in a code span) ·
`git` (lowercase, 25/25) · `Docker` · `Apple Container` · `Cloudflare Tunnel` ·
`Cloudflare Pages` · `launchd` (lowercase, 28/28) · `systemd` · `Firecracker` ·
`VitePress` · `Node` · `pnpm` · `Turborepo` · `wrangler`

## Long vowel marks in Japanese

**Not written.** `サーバ` 10 against `サーバー` 1; `ブラウザ`, `プロキシ`,
`ディレクトリ` likewise. So `サーバ`, `ユーザ`, `ブラウザ`, `コンピュータ`.

This follows the corpus rather than the JTF style guide, which would write
`サーバー`. If that is ever preferred, this section is the only place that has
to change — `japanese.md` defers to it.
