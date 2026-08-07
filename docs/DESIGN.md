# Minato 設計ドキュメント

AI エージェント向けの開発環境管理ツール。git worktree を作ったら即座にプレビュー環境が立ち上がる状態を作る。

## 1. コンセプト

**「worktree = 環境」の 1:1 対応を不変条件にする。**

worktree が生まれたら環境が生まれ、消えたら消える。エージェントはこの対応関係さえ知っていれば、自分が今どの環境を見ているのか、変更をどう確認すればいいのかを迷わない。

```
~/ghq/github.com/hota1024/myapp/            # main worktree   → myapp.localhost
~/ghq/github.com/hota1024/myapp.wt/feat-1/  # feat-1 worktree → feat-1.myapp.localhost
                                  /feat-2/  # feat-2 worktree → feat-2.myapp.localhost

https://web.feat-1.myapp.localhost   → feat-1 の web コンテナ :3000
https://api.feat-1.myapp.localhost   → feat-1 の api コンテナ :8080
```

エージェントに提供する体験は次の 3 つに集約される。

1. `minato new feat-1` で環境ごとブランチが生える
2. `minato url web` で確認先の URL が手に入る
3. `minato logs` / `minato exec` で中を覗ける（`docker` コマンドを直接触らせない）

## 2. 用語

| 用語 | 意味 |
| --- | --- |
| **Project** | git リポジトリ。main worktree の `origin` URL、なければ絶対パスで同定する |
| **Workspace** | worktree 1 つに対応する環境。名前は worktree 名（ブランチ名をサニタイズしたもの） |
| **Service** | Workspace 内の 1 プロセス（web, api, db …） |
| **Runtime** | 仮想化バックエンド（Docker / Firecracker / Apple Container） |
| **Supervisor** | daemon 内で Workspace のライフサイクルを管理するコンポーネント |

「Environment」という語は環境変数と紛らわしいので使わない。

## 3. アーキテクチャ

```
  minato (CLI) ──────┐
  minato-desktop ────┼─── Unix socket / JSON-RPC ───┐
  SKILL.md (agent) ──┘                              │
        └── いずれも minato-client 経由              ▼
                                            ┌───────────┐
                                            │  minatod  │
                                            │ Supervisor│
                                            └─────┬─────┘
              ┌──────────────┬───────────────┼──────────────┬──────────────┐
              ▼              ▼               ▼              ▼              ▼
         DNS (:53)     Proxy (:80/:443)   Runtime      Env Resolver     Tunnel
      hickory-server   hyper + rustls    ┌──trait──┐   3層マージ +    cloudflared
      *.localhost →     Host ベース      │ Docker  │   シークレット参照   ingress
        127.0.0.1        振り分け        │Firecracker│
                                         │ Apple   │
                                         └─────────┘
```

**daemon を置く**。ポート台帳・リバースプロキシ・DNS・Tunnel・アイドル監視はいずれも常駐が要る。

### 原則: daemon の API が製品の本体

CLI・GUI・Skills はいずれも daemon の**同格のクライアント**であり、表層に過ぎない。ロジックはクライアント側に一切置かない。

この原則は M0 の時点から守る必要がある。CLI 専用の都合（人間向けの整形、対話的な確認、進捗表示）を daemon の API に混ぜると、GUI 実装時に必ず破綻する。具体的には次を守る。

- daemon の応答は常に構造化データ。人間向け文字列を返さない
- 長時間処理（起動、ビルド）は**イベントストリームを返す**。CLI はそれをプログレス表示に、GUI は同じものを進捗バーに変換する
- 確認プロンプトは daemon が出さない。破壊的操作は `force` フラグをクライアントが渡す

イベントストリームを M0 から用意しておく点が特に重要で、後付けにすると `minato up` が完了までブロックする設計になってしまい、GUI で「起動中の様子」を出せなくなる。

### なぜ daemon か

| 責務 | 常駐が必要な理由 |
| --- | --- |
| Proxy | :80/:443 を占有し続ける必要がある |
| DNS | :53 を占有し続ける必要がある |
| Supervisor | scale-to-zero のアイドル判定に時間軸が要る |
| Port 台帳 | 複数 workspace 間のポート衝突を防ぐ single source of truth |
| Tunnel | cloudflared プロセスの寿命管理 |

## 4. 環境定義: `minato.toml`

独自スキーマを第一級とする。Docker は「バックエンドの一つ」であり、compose は内部生成の実装詳細に落とす。これにより Firecracker 対応時に定義フォーマットを作り直さずに済む。

プロジェクトルート（main worktree）に置いてコミットする。worktree 固有の上書きは `minato.local.toml`（gitignore 対象）。

```toml
[project]
name = "myapp"
# domain = "myapp.localhost"   # 省略時は name から導出

[runtime]
default = "docker"

[services.web]
build = "./web"                 # or image = "node:22"
port = 3000
command = "pnpm dev"
health = "http://localhost:3000/healthz"   # scale-to-zero の起動完了判定
idle_timeout = "30m"
env = { NODE_ENV = "development" }

[services.api]
build = "./api"
port = 8080
depends_on = ["db"]
health = "tcp://localhost:8080"

[services.db]
image = "postgres:16"
port = 5432
scope = "project"               # worktree 間で共有（デフォルトは "workspace"）
volumes = ["pgdata:/var/lib/postgresql/data"]
expose = false                  # URL を生やさない（内部通信のみ）
```

### 設計上の要点

**`scope`**: `workspace`（既定）は worktree ごとに独立したインスタンスを立てる。`project` は同一プロジェクトの全 worktree で 1 インスタンスを共有する。DB を worktree ごとに立てると seed とリソースが辛いので、共有できる余地を最初から用意しておく。

**`expose`**: 既定は `port` があれば true。DB のような内部サービスは `false` にして URL を生やさない。

**`health`**: scale-to-zero の要。これがないと「起動したがまだ受け付けない」状態でプロキシが 502 を返す。`http://`, `tcp://`, `cmd:` の 3 形式をサポートする。

**サービス間の名前解決**: 同一 workspace 内では runtime 側のネットワークでサービス名（`db:5432`）が引ける。異なる scope をまたぐ場合（workspace の api → project の db）は daemon がエイリアスを張る。

## 5. 命名とルーティング

### ホスト名の構成

```
{service}.{workspace}.{project}.localhost
```

main worktree は workspace ラベルを省略して `{service}.{project}.localhost` とする。

### サニタイズ規則

ブランチ名は DNS ラベルとして使えない文字を含む。

1. 小文字化し、`[a-z0-9-]` 以外を `-` に置換（`feature/user-auth` → `feature-user-auth`）
2. 連続する `-` を 1 つに畳み、先頭末尾の `-` を除去
3. 63 文字を超える場合は 55 文字に切り詰め、元の名前の SHA-256 先頭 7 文字を `-` 区切りで付与
4. サニタイズ後に既存 workspace と衝突する場合も同様にハッシュを付与

サニタイズ結果は状態ストアに永続化し、以後は再計算せず参照する（規則を変えても既存環境の URL が変わらないようにするため）。

### DNS

macOS では `*.localhost` はシステムレベルでは解決されない。Chrome は独自に 127.0.0.1 へ解決するが、`curl` / Safari / Node.js の fetch は解決しない。**エージェントは curl で疎通確認する**ので、これは致命的。

対策として daemon 内に DNS サーバを持ち、`/etc/resolver/localhost` に `nameserver 127.0.0.1` を書く。これは `minato init` で一度だけ sudo を要求する。

```
/etc/resolver/localhost:
  nameserver 127.0.0.1
  port 53
```

Linux では `systemd-resolved` が `.localhost` を解決するため、DNS サーバは任意（`.test` など別 TLD を使う場合のみ必要）。

### Proxy と TLS

hyper ベースのリバースプロキシが :80/:443 で待ち受け、Host ヘッダ（HTTPS は SNI）でルーティングする。

ローカル CA を `~/.minato/ca/` に生成し、`*.localhost` および `*.{project}.localhost` のワイルドカード証明書に署名する。CA 証明書はシステムキーチェーンに登録する（`minato init` 時、sudo）。

**WebSocket / SSE / HTTP/2 は必ず通す**。開発サーバの HMR がこれに依存しているため、ここが動かないと使いものにならない。

### 特権ポート

macOS では非 root プロセスは 1024 未満をバインドできない。launchd の socket activation を使い、launchd（root）が :53 / :80 / :443 をバインドしてファイルディスクリプタを daemon に渡す。daemon 本体は非 root で動かせる。

Linux では `CAP_NET_BIND_SERVICE` または systemd socket activation を使う。

## 6. Runtime 抽象

Runtime は「サービス 1 つを起動して待ち受けアドレスを返すもの」として定義する。ネットワークの結線とルーティングは Minato 側が持つ。これにより Docker の compose や network の概念に抽象が引きずられない。

```rust
#[async_trait]
pub trait Runtime: Send + Sync {
    fn id(&self) -> &'static str;

    /// イメージ/rootfs のビルド、ネットワーク・ボリュームの用意
    async fn prepare(&self, ws: &WorkspaceSpec) -> Result<()>;

    async fn start(&self, svc: &ServiceSpec) -> Result<RunningService>;
    async fn stop(&self, id: &ServiceId) -> Result<()>;
    async fn destroy(&self, ws: &WorkspaceSpec) -> Result<()>;

    async fn exec(&self, id: &ServiceId, cmd: &[String]) -> Result<ExecResult>;
    async fn logs(&self, id: &ServiceId, opts: LogOptions) -> Result<BoxStream<'_, LogLine>>;
    async fn inspect(&self, id: &ServiceId) -> Result<ServiceState>;
}

pub struct RunningService {
    pub id: ServiceId,
    /// Proxy がここへ転送する。Docker なら 127.0.0.1:<動的ポート>、
    /// Firecracker なら VM の tap インタフェース上の IP:port
    pub endpoint: SocketAddr,
}
```

`endpoint: SocketAddr` を返すのが要点。プロキシは Runtime の実装を一切知らずに済む。

### バックエンド別の対応状況

| Runtime | 対象 OS | 位置づけ |
| --- | --- | --- |
| Docker | macOS / Linux | v0 のデフォルト。`bollard` で Docker API を直接叩く（compose CLI は呼ばない）。M0 で実装済み |
| Apple Container | macOS 15+ | `container` CLI を叩く。M0 で実装済み（実機未検証） |
| Firecracker | **Linux のみ** | 高密度・強い分離が要る場合。macOS では動かない |

### Docker と Apple Container の構造的な差（M0 で判明）

両方を実装したことで、Runtime 抽象が吸収すべき差が具体的になった。

| | Docker | Apple Container |
| --- | --- | --- |
| 操作方法 | HTTP API（bollard） | CLI (`container`) |
| ポート | ホストへ動的フォワード（`127.0.0.1:49312`） | **各コンテナが専用 IP**（`192.168.64.3:3000`）。publish 不要 |
| 絞り込み | API 側でラベルフィルタ | フィルタ不可。全件取得して手元で絞る |
| ネットワーク | 任意に作成可能 | **macOS 26 以降のみ**。それ以前は既定ネットワークに相乗り |
| サービス名の解決 | ネットワークエイリアスで `db:5432` | エイリアスなし。`{コンテナ名}.test` でのみ引ける |
| 名前付きボリューム | ネイティブ対応 | 概念がない。`~/.minato/volumes/` の bind mount に写像 |

**`RunningService::endpoint: SocketAddr` を返す設計がここで効いた。** Docker はホストのフォワードポート、Apple Container はコンテナ自身の IP を返すが、プロキシと Supervisor はその違いを一切知らない。ポートフォワードを前提にした型（`host_port: u16`）にしていたら Apple Container 対応で作り直しになっていた。

サービス名の解決だけは抽象化しきれず、`ServiceSpec::peers` を追加した。Apple Container はこれを使って `MINATO_HOST_<SERVICE>` を注入し、相手のホスト名をアプリに伝える。Docker では未使用。

**注意**: Firecracker は KVM 依存で macOS では動作しない。開発機が macOS なので、Firecracker サポートは Linux サーバ上の Minato（リモートホスト運用）か、macOS では Apple Container / krunkit を代替とする前提で設計する。Runtime trait はこの差を吸収するために存在する。

## 7. 起動戦略: scale-to-zero + オンデマンド

worktree を 10 個作っても全部を常時起動しない。これが Minato の差別化点であり、「気軽に worktree を作れる」体験の前提になる。

### 状態機械

```
Stopped ──(リクエスト到達)──> Starting ──(health OK)──> Ready
   ▲                              │                        │
   └──(idle_timeout 経過)── Idle <─┘ (health NG/timeout)    │
                              ▲                            │
                              └────(無アクセス継続)─────────┘
```

### 起動待ちのハンドリング

初回リクエストは数秒〜数十秒待たされる。クライアント種別で挙動を変える。

| クライアント | 挙動 |
| --- | --- |
| ブラウザ（`Accept: text/html`） | 即座に「起動中」ページを 200 で返し、SSE で Ready を待って自動リロード |
| API / curl / エージェント | Ready まで待ってから転送。最大 120 秒でタイムアウトし 504 |

エージェントの `curl` は待たせるのが正解。中途半端にエラーを返すとエージェントが誤った判断（「サーバが壊れている」）をする。

### 起動を速くする施策

- ベースイメージは project スコープでビルドし、worktree 間で共有する
- worktree 固有の差分はソースコードの bind mount のみに限る
- `node_modules` などは named volume を workspace ごとに持ち、初回のみインストール
- `minato new` の時点で先行して `prepare` を走らせる（`--no-warm` で無効化）

### 状態の正は runtime のラベル（M0 で確定）

daemon は**実行中の状態を状態ファイルに持たない**。コンテナのラベル（`dev.minato.*`）が唯一の正であり、daemon が再起動しても `list_project` の結果だけで全状態を復元できる。

状態ストアが持つのは「どの worktree を Minato が管理しているか」と「その worktree に発行した URL ラベル」だけ。ラベルを永続化するのは、[命名規則](#5-命名とルーティング)を将来変更しても既存 workspace の URL が変わらないようにするため。

この設計により、未決事項に挙げていた「クラッシュ後の実コンテナとの reconcile」がほぼ消える。突き合わせるべき二重の状態が存在しない。

### 「起動した」と「受け付けられる」は別（M0 で判明）

コンテナが起動しても、中のアプリはまだ listen していない。`up` が返った直後に `curl` すると connection refused になる。

人間なら「まだかな」と数秒待つが、**エージェントは 1 回失敗した時点で「サーバが壊れている」と判断してしまう**。エージェント向けツールとしてこれは致命的なので、M0 の時点で TCP 接続が通るまで待つ処理を入れた（`readiness::await_service`、上限 15 秒）。

上限を超えた場合は待たずに進み、警告だけ出す。開発サーバの初回起動は依存解決やコンパイルでこれより長くかかることがあり、無限に待つと `up` が返らなくなるため。

M2 ではこれを `minato.toml` の `health`（HTTP のステータス、コマンドの終了コード）による本来の判定に置き換える。今の実装はその下地。

### 停止中のコンテナは作り直す

`up` は既に動いているコンテナには手を触れない（何度叩いても同じ結果になる）。一方、**停止中のコンテナは削除して作り直す**。設定を変えたのに反映されない方が、起動が数秒遅いことより混乱を招くため。

副作用として、`down` → `up` でホスト側のポート番号が変わる。M1 で URL が固定されるまでの間だけ表に出る性質。

## 8. 環境変数管理

### 3 層マージ

後勝ちで解決する。

```
1. global     ~/.minato/env                     全プロジェクト共通
2. project    minato.toml の env + .minato/env  リポジトリにコミット
3. workspace  .minato/env.local                 gitignore、worktree 固有
```

### シークレット

平文をリポジトリに入れない。値に参照形式を書けるようにし、起動時に解決する。

```
DATABASE_PASSWORD = "op://Development/myapp/password"   # 1Password CLI
API_KEY           = "keychain://minato/myapp/api-key"   # macOS Keychain
STRIPE_KEY        = "env://STRIPE_KEY"                  # daemon の環境変数から
```

解決した値は daemon のメモリ上にのみ置き、ディスクに書かない。`minato env ls` はマスクして表示する。

### 自動注入される変数

全サービスに以下を注入する。**フロントエンドが API の URL を知る手段がないと worktree ごとの環境は成立しない**ため、これは必須機能。

```
MINATO_PROJECT       = myapp
MINATO_WORKSPACE     = feat-1
MINATO_SERVICE       = web
MINATO_URL_WEB       = https://web.feat-1.myapp.localhost
MINATO_URL_API       = https://api.feat-1.myapp.localhost
MINATO_TUNNEL_URL_WEB = https://web-feat-1.myapp.example.com   # Tunnel 有効時
```

## 9. Cloudflare Tunnel

### 方式

named tunnel を **マシン単位で 1 本**張る。ingress ルールは `http://127.0.0.1:80` へ全部流し、Host ヘッダによる振り分けはローカルプロキシに任せる。

DNS 側は `*.{project}.example.com` のワイルドカード CNAME を 1 本作るだけで済み、workspace が増減してもレコード操作が発生しない。これが最も単純で、起動レイテンシにも影響しない。

```yaml
# 生成される cloudflared 設定
tunnel: <tunnel-id>
ingress:
  - hostname: "*.myapp.example.com"
    service: http://127.0.0.1:80
    originRequest:
      httpHostHeader: <リクエストの Host を localhost 名に書き換え>
  - service: http_status:404
```

Tunnel 側のホスト名はドット区切りのサブドメインが 1 段しか使えない場合があるため、`{service}-{workspace}.{project}.example.com` の形にする。

### アクセス制御

既定で Cloudflare Access のポリシーを張る。開発環境が無認証でインターネットに露出するのは事故なので、**opt-out（`--public`）にする**。

## 10. CLI

全コマンドが `--json` を持ち、終了コードと構造化出力だけで完結する。エージェントが人間向けの出力をパースする必要をなくす。

```
minato init                       # minato.toml 生成 + daemon/DNS/CA セットアップ
minato doctor                     # DNS resolver / docker / 証明書 / ポート占有の診断

minato new <branch> [--from main] # git worktree add + 環境の warm-up + URL 表示
minato rm <workspace> [--force]   # 環境破棄 + git worktree remove
minato ls [--json]                # workspace 一覧と状態

minato up [--service web]         # 明示起動
minato down [--all]
minato restart <service>
minato status [--json]

minato url [service]              # URL を stdout に 1 行で
minato open [service]             # ブラウザで開く
minato logs <service> [-f] [--tail N] [--since 5m]
minato exec <service> -- <cmd>

minato env set KEY=VAL [--scope global|project|workspace]
minato env ls [--json] [--reveal]
minato env unset KEY

minato tunnel enable [--domain example.com] [--public]
minato tunnel disable
minato tunnel status
```

`minato doctor` は初期設定に sudo と外部依存（Docker, cloudflared）が絡むため優先度が高い。失敗時に「何をすればいいか」を出す。

### JSON 出力の例

```jsonc
// minato status --json
{
  "project": "myapp",
  "workspace": "feat-1",
  "branch": "feature/user-auth",
  "path": "/Users/hotaka/ghq/github.com/hota1024/myapp.wt/feat-1",
  "services": [
    {
      "name": "web",
      "state": "ready",
      "url": "https://web.feat-1.myapp.localhost",
      "tunnel_url": "https://web-feat-1.myapp.example.com",
      "endpoint": "127.0.0.1:49312",
      "last_access": "2026-08-07T09:12:44Z"
    },
    { "name": "db", "state": "ready", "url": null, "scope": "project" }
  ]
}
```

## 11. Skills

`minato skill install` で `.claude/skills/minato/SKILL.md` を配置する。CLI のリファレンスではなく、**判断基準**を書く。

- 変更を確認したいときは `minato url <service>` で URL を取得してから確認する。ポート番号を推測しない
- ログは `minato logs`。`docker logs` / `docker ps` を直接使わない
- 環境が応答しない場合はまず `minato status --json` で状態を見る。`starting` なら待つ
- 環境変数の追加は `minato env set`。`.env` を直接書かない
- 新しいブランチで作業を始めるときは `minato new <branch>`

MCP サーバは当面作らない。CLI が `--json` を持つ以上、Bash 経由で十分に扱えるため、二重メンテのコストに見合わない。

## 12. GUI (`minato-desktop`)

egui / eframe による純 Rust の GUI。`minato-client` を直接リンクするため、型定義の共有に生成ステップが要らない（TypeScript を使わない選択の最大の利点）。

### 想定する画面

immediate mode UI は「頻繁に更新される一覧」と相性が良く、Minato の GUI に必要なものはほぼそれに収まる。

1. **Workspace 一覧** — project / workspace / サービスの状態（`stopped` / `starting` / `ready` / `idle`）とリソース使用量を常時更新
2. **URL パネル** — クリックでコピー、ブラウザで開く、Tunnel URL の切り替え
3. **ログビューア** — サービス横断の tail、フィルタ
4. **環境変数エディタ** — 3 層のどこで定義された値かを可視化（シークレットはマスク）
5. **doctor** — DNS resolver / 証明書 / ポート占有の診断と、ワンクリック修復

### メニューバー常駐

Minato の GUI は常時開くものではなく、「今どの環境が動いているか」を確認して開く用途が主になる。egui 単体では tray を扱えないため `tray-icon` crate を併用し、次の構成をとる。

- 常駐は tray アイコンのみ。メニューから起動中の workspace と URL に直接アクセスできる
- ウィンドウは要求されたときだけ開く（`eframe` を遅延起動）
- ウィンドウを閉じてもプロセスは終了しない

### 非同期の扱い

egui の描画ループは同期的で、`async` を直接扱えない。以下の構造で分離する。

```
[tokio runtime スレッド]                    [egui 描画スレッド]
  minato-client で daemon を購読              AppState を読んで描画
  受信したイベントを AppState に反映   ───>   ユーザー操作をコマンドとして送出
       (Arc<RwLock<AppState>> + ctx.request_repaint())
```

daemon 側でイベントストリームを用意してある（§3）ため、GUI はポーリングせずに済む。`request_repaint()` はイベント受信時のみ呼び、アイドル時は再描画しない。

### 既知の注意点

- **日本語フォント**: egui はデフォルトで CJK グリフを持たない。フォントを埋め込む必要がある（ブランチ名やパスに日本語が入りうる）
- **表現力**: ネイティブ感は劣る。UI の作り込みに時間をかけず、情報密度と更新の速さで勝負する方針をとる

## 13. リポジトリ構成

Cargo workspace 単体で完結する。egui を選んだため Node.js / pnpm のツールチェーンは不要で、`packages/`（TS）は設けない。

```
minato/
├── Cargo.toml            # [workspace] members + workspace.dependencies
├── rust-toolchain.toml
├── crates/               # ライブラリ（出荷しない）
│   ├── minato-core/      #   spec, config, naming, state store（依存グラフの底）
│   ├── minato-api/       #   RPC のリクエスト/レスポンス/イベント型（単一ソース）
│   ├── minato-client/    #   RPC クライアント。CLI と GUI が共有
│   ├── minato-runtime/   #   Runtime trait + docker 実装
│   ├── minato-proxy/     #   hyper リバースプロキシ + rustls + ローカル CA
│   ├── minato-dns/       #   hickory-server ラッパー
│   └── minato-tunnel/    #   cloudflared プロセス管理
├── apps/                 # バイナリ（出荷する）
│   ├── daemon/           #   minatod — Supervisor + RPC サーバ
│   ├── cli/              #   minato
│   └── desktop/          #   minato-desktop — egui GUI
├── skills/
│   └── minato/SKILL.md
├── xtask/                # cargo xtask（ビルド・パッケージング・launchd plist 生成）
└── docs/
    └── DESIGN.md
```

### 依存の方向

```
apps/cli ────┐
apps/desktop ┴──> minato-client ──> minato-api ──> minato-core
apps/daemon ─────────────────────>  minato-api ──> minato-core
       └──> minato-runtime / minato-proxy / minato-dns / minato-tunnel ──> minato-core
```

`minato-api` が daemon とクライアントの唯一の接点。**クライアント側の crate が `minato-runtime` などに依存してはいけない**（依存すると GUI に Docker のロジックが漏れ、daemon 経由という原則が崩れる）。この制約は CI で `cargo-deny` ないし依存グラフの検査で守る。

### バージョニング

全 crate を単一バージョンで揃える（`workspace.package.version` を継承）。内部 crate を個別に crates.io へ公開する予定がないため、独立バージョニングの複雑さは不要。公開するのは `minato`（CLI）のみ。

### 主要な依存クレート

| 用途 | クレート |
| --- | --- |
| 非同期ランタイム | `tokio` |
| CLI | `clap` (derive) |
| 設定 | `serde`, `toml`, `figment` |
| Docker API | `bollard` |
| HTTP / Proxy | `hyper`, `hyper-util`, `axum`（管理 API 用） |
| TLS | `rustls`, `rcgen`（ローカル CA） |
| DNS | `hickory-server` |
| IPC | `tokio::net::UnixListener` |
| GUI | `eframe`, `egui`, `egui_extras`, `tray-icon` |
| ログ | `tracing`, `tracing-subscriber` |
| エラー | `thiserror`（ライブラリ）, `anyhow`（バイナリ） |
| Git | `gix` または `git` コマンド呼び出し |

## 14. ロードマップ

| マイルストーン | 内容 | 完了条件 |
| --- | --- | --- |
| **M0** ✅ | workspace 骨組み + core（config / naming / state）+ `minato-api`（イベントストリーム含む）+ Docker / Apple Container runtime + `init` / `new` / `up` / `down` / `rm` / `ls` / `status` / `url` / `daemon` | worktree を作るとコンテナが起動し、`localhost:<動的ポート>` で見える |
| **M1** | DNS + Proxy + TLS + `doctor` | `https://web.feat-1.myapp.localhost` が curl で通る |
| **M2** | scale-to-zero + health check + アイドル停止 | worktree 10 個作っても実行中コンテナは触っているものだけ |
| **M3** | 環境変数管理（3 層 + シークレット参照 + 自動注入） | `MINATO_URL_API` がフロントから読める |
| **M4** | Cloudflare Tunnel | スマホから `https://web-feat-1.myapp.example.com` が見える |
| **M5** | Skills | エージェントが `docker` を直接触らずに開発を完了できる |
| **M6** | GUI（egui + tray） | メニューバーから起動中の workspace と URL が見え、ログが読める |
| **M7** | Runtime 追加（Apple Container / Firecracker） | `[runtime] default` の切り替えだけで動く |

M1 完了時点が最小の価値提供ライン。M2 まで行くと日常的に使える。

GUI を M6 に置いたのは、daemon の API がひととおり出揃ってからの方が手戻りが少ないため。ただし **API のイベントストリームだけは M0 で用意する**（後付けするとブロッキング前提の設計になり、GUI で進捗を出せなくなる）。

### M0 で先送りしたもの

| 項目 | 先送りした理由 | 予定 |
| --- | --- | --- |
| `build`（Dockerfile のビルド） | 既製イメージ + bind mount の方が起動が速く、「すぐ立ち上がる」思想に合う。ビルドコンテキストの tar 化も必要 | M0.5 |
| `Request::Cancel` | プロトコルには入れたが daemon 側が未対応。長時間処理が prepare/start に限られ、中断の要求が出ていない | M2 |
| `minato logs` / `exec` | Runtime trait には口があるが CLI 未実装 | M0.5 |
| `ls --all-projects` | 他プロジェクトの `minato.toml` の場所を状態ストアが持っていない | M3 |
| `minato.local.toml` の上書き | 環境変数管理と同時に入れる方が設計が揃う | M3 |

## 15. 未決事項

- **共有 DB のマイグレーション衝突**: `scope = "project"` の DB に対し、複数 worktree が別々のマイグレーションを当てると壊れる。worktree ごとに database を切る（同一インスタンス内で DB 名を分ける）案が有力だが、Runtime 非依存に実装する方法が未定
- **`minato.toml` の自動生成精度**: `minato init` で既存プロジェクトから推定生成したい（compose / package.json / Dockerfile を読む）。どこまで自動化するか
- **worktree のディレクトリ規約**: `{repo}.wt/{name}` を既定とするが、既存の運用（ghq 配下、`.git/worktrees` 隣接など）とどう折り合うか
- **daemon の複数プロジェクト同時管理**: 1 daemon が全プロジェクトを見る前提だが、状態ストアのスキーマとロック戦略が未定
- **`MINATO_HOME` のパス長**: Unix socket の `sun_path` は macOS で 104 バイト。深い場所を指定すると bind が失敗するため `Paths::check_socket_length()` で事前に弾いているが、そもそも socket を `$TMPDIR` に逃がす手もある
- **GUI と daemon のライフサイクル**: GUI 起動時に daemon が落ちていたら自動起動するか。GUI を「daemon の監視役」にすると責務が二重になるため、launchd に任せて GUI は接続待ちに徹する案が有力
- **GUI の配布形態**: `.app` バンドルとして署名・公証するか、`cargo install` で済ませるか。tray 常駐する以上、前者が望ましいが公証のコストがかかる
