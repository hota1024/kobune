//! 仮想化バックエンドの共通インタフェース。
//!
//! この trait が返す [`RunningService::endpoint`] は「プロキシが転送する先」
//! であって、それがホストのフォワードポートなのかコンテナ自身の IP なのかは
//! 実装の裁量に委ねる。プロキシと Supervisor はその差を知らない。

use async_trait::async_trait;
use futures::stream::BoxStream;
use minato_api::OutputStream;

use crate::error::Result;
use crate::event::EventSink;
use crate::spec::{
    RunningService, ServiceKey, ServiceSpec, ServiceStatus, WorkspaceKey, WorkspaceSpec,
};

/// ログの取り方。
#[derive(Debug, Clone, Default)]
pub struct LogOptions {
    /// 新しい行を待ち続ける。
    pub follow: bool,
    /// 末尾から何行取るか。`None` は全部。
    pub tail: Option<usize>,
}

/// ログ 1 行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    pub stream: OutputStream,
    pub line: String,
}

/// コンテナ内でコマンドを実行した結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutcome {
    /// 実行したコマンドの終了コード。
    ///
    /// **そのまま呼び出し元に伝える。** `minato exec web -- pnpm test` の
    /// 成否をエージェントが終了コードで判定できる必要がある。
    pub exit_code: i32,
}

/// runtime の素性。`minato doctor` と `minato ping` が表示する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeInfo {
    pub id: String,
    pub version: String,
    /// ネットワークを自前で作れるか。
    ///
    /// Apple Container では macOS 26 未満だとネットワークを作成できず、
    /// 既定のネットワークに相乗りするしかない。
    pub supports_custom_networks: bool,
}

/// 仮想化バックエンド。
#[async_trait]
pub trait Runtime: Send + Sync {
    /// `minato.toml` の `[runtime] default` に書く識別子。
    fn id(&self) -> &'static str;

    /// 接続できるか、使えるバージョンかを確認する。
    async fn probe(&self) -> Result<RuntimeInfo>;

    /// workspace 単位の下準備。ネットワークとボリュームの用意、イメージの取得。
    async fn prepare(&self, spec: &WorkspaceSpec, events: &EventSink) -> Result<()>;

    /// サービスを起動する。既に起動していれば何もせず現状を返す。
    async fn start(&self, spec: &ServiceSpec, events: &EventSink) -> Result<RunningService>;

    /// サービスを停止する。コンテナは残す（次回の起動を速くするため）。
    async fn stop(&self, key: &ServiceKey, events: &EventSink) -> Result<()>;

    /// サービスのコンテナを削除する。
    async fn remove(&self, key: &ServiceKey, events: &EventSink) -> Result<()>;

    /// workspace に属するものをすべて片付ける。ネットワークも消す。
    ///
    /// 共有サービス（`scope = "project"`）は他の workspace が使っているため対象外。
    async fn destroy_workspace(&self, key: &WorkspaceKey, events: &EventSink) -> Result<()>;

    /// 1 サービスの現在の状態。
    async fn inspect(&self, key: &ServiceKey) -> Result<ServiceStatus>;

    /// ログを読む。
    ///
    /// これが無いとエージェントは `docker logs` に戻るしかなくなる。
    async fn logs(
        &self,
        key: &ServiceKey,
        options: LogOptions,
    ) -> Result<BoxStream<'static, LogLine>>;

    /// コンテナ内でコマンドを実行する。
    ///
    /// 出力は `events` に流し、終了コードを返す。TTY は要求しない
    /// （エージェントが使う用途では対話が発生しない方が安全）。
    async fn exec(
        &self,
        key: &ServiceKey,
        command: &[String],
        events: &EventSink,
    ) -> Result<ExecOutcome>;

    /// プロジェクトに属する Minato 管理下のサービスをすべて列挙する。
    ///
    /// daemon はこれを使って再起動後の状態を復元する。状態ストアではなく
    /// runtime が状態の正であるため、この一覧が信頼できる必要がある。
    async fn list_project(&self, project: &str) -> Result<Vec<ServiceStatus>>;
}

/// コンテナに付けるラベルのキー。
///
/// runtime をまたいで同じキーを使う。daemon はこのラベルだけを頼りに
/// 自分の管理下のコンテナを判別する。
pub mod labels {
    /// Minato が作ったことを示す。値は `"1"`。
    pub const MANAGED: &str = "dev.minato.managed";
    pub const PROJECT: &str = "dev.minato.project";
    /// workspace ラベル。共有サービスでは `_shared`。
    pub const WORKSPACE: &str = "dev.minato.workspace";
    pub const SERVICE: &str = "dev.minato.service";
    /// `workspace` または `project`。
    pub const SCOPE: &str = "dev.minato.scope";
    /// コンテナ内で待ち受けるポート。
    pub const PORT: &str = "dev.minato.port";

    pub const MANAGED_VALUE: &str = "1";
}

/// 共通の命名規則。runtime 実装はこれに従う。
pub mod names {
    use crate::spec::{ServiceKey, WorkspaceKey};

    /// コンテナ名。
    ///
    /// 人間が `docker ps` や `container ls` で見て意味が分かる形にする。
    pub fn container(key: &ServiceKey) -> String {
        format!(
            "minato-{}-{}-{}",
            key.workspace.project,
            sanitize_segment(&key.workspace.workspace),
            key.service
        )
    }

    /// workspace ごとのネットワーク名。
    pub fn network(key: &WorkspaceKey) -> String {
        format!(
            "minato-{}-{}",
            key.project,
            sanitize_segment(&key.workspace)
        )
    }

    /// 名前付きボリュームの実体名。プロジェクト間で衝突させない。
    pub fn volume(project: &str, name: &str) -> String {
        format!("minato-{project}-{name}")
    }

    /// `_shared` の先頭 `_` はコンテナ名に使えない実装があるため落とす。
    fn sanitize_segment(segment: &str) -> String {
        segment.trim_start_matches('_').to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::WorkspaceKey;

    #[test]
    fn container_names_are_readable() {
        let key = WorkspaceKey::new("myapp", "feat-1").service("web");
        assert_eq!(names::container(&key), "minato-myapp-feat-1-web");
    }

    #[test]
    fn shared_services_get_a_usable_container_name() {
        let key = WorkspaceKey::shared("myapp").service("db");
        let name = names::container(&key);

        assert_eq!(name, "minato-myapp-shared-db");
        assert!(
            !name.contains('_'),
            "コンテナ名に `_` を含めない実装があるため落とす: {name}"
        );
    }

    #[test]
    fn networks_are_scoped_per_workspace() {
        let a = names::network(&WorkspaceKey::new("myapp", "feat-1"));
        let b = names::network(&WorkspaceKey::new("myapp", "feat-2"));
        assert_ne!(a, b);
        assert_eq!(a, "minato-myapp-feat-1");
    }

    #[test]
    fn volumes_are_scoped_per_project() {
        assert_eq!(names::volume("myapp", "pgdata"), "minato-myapp-pgdata");
        assert_ne!(
            names::volume("myapp", "pgdata"),
            names::volume("other", "pgdata"),
            "プロジェクトが違えば別の領域になる"
        );
    }
}
