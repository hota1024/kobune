//! runtime に渡す仕様。特定の実装に依存しない語彙で書く。
//!
//! ここに Docker 固有の概念（compose、network driver）を持ち込むと
//! Apple Container / Firecracker の実装が歪む。

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use minato_core::{HealthCheck, ServiceScope, ServiceState};

/// `scope = "project"` のサービスが属する仮想的な workspace 名。
///
/// 共有インスタンスは特定の worktree に属さないため、ラベル上は
/// この予約名を使う。`_` 始まりは DNS ラベルとして無効なので、
/// 実在の workspace 名と衝突しない。
pub const SHARED_WORKSPACE: &str = "_shared";

/// workspace を一意に指す。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorkspaceKey {
    pub project: String,
    pub workspace: String,
}

impl WorkspaceKey {
    pub fn new(project: impl Into<String>, workspace: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            workspace: workspace.into(),
        }
    }

    /// `scope = "project"` のサービスが属するキー。
    pub fn shared(project: impl Into<String>) -> Self {
        Self::new(project, SHARED_WORKSPACE)
    }

    pub fn is_shared(&self) -> bool {
        self.workspace == SHARED_WORKSPACE
    }

    pub fn service(&self, service: impl Into<String>) -> ServiceKey {
        ServiceKey {
            workspace: self.clone(),
            service: service.into(),
        }
    }
}

/// サービスのインスタンスを一意に指す。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ServiceKey {
    pub workspace: WorkspaceKey,
    pub service: String,
}

impl std::fmt::Display for ServiceKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}/{}/{}",
            self.workspace.project, self.workspace.workspace, self.service
        )
    }
}

/// 1 つの workspace をまとめて扱うための仕様。
#[derive(Debug, Clone)]
pub struct WorkspaceSpec {
    pub key: WorkspaceKey,
    /// worktree のパス。コンテナにマウントする元。
    pub worktree_path: PathBuf,
    pub services: Vec<ServiceSpec>,
}

impl WorkspaceSpec {
    pub fn service(&self, name: &str) -> Option<&ServiceSpec> {
        self.services.iter().find(|s| s.key.service == name)
    }
}

/// サービス 1 つの起動仕様。
#[derive(Debug, Clone)]
pub struct ServiceSpec {
    /// `scope = "project"` の場合、`key.workspace` は [`WorkspaceKey::shared`]。
    pub key: ServiceKey,

    /// このサービスを必要としている workspace。
    ///
    /// 共有サービスでも、呼び出し元の workspace ネットワークに繋ぐ必要がある。
    pub attached_to: WorkspaceKey,

    pub image: String,
    pub command: Option<Vec<String>>,
    pub workdir: String,
    pub env: BTreeMap<String, String>,

    /// コンテナ内で待ち受けるポート。
    pub port: Option<u16>,

    /// 受け付け可能かどうかの判定方法。
    ///
    /// 指定が無ければ TCP 接続の可否で判断する。
    pub health: Option<HealthCheck>,

    pub scope: ServiceScope,
    pub volumes: Vec<VolumeMount>,

    /// worktree のソースをマウントする指定。共有サービスでは `None`。
    pub source_mount: Option<SourceMount>,

    /// 同じ workspace で動く他のサービス名。
    ///
    /// Docker はネットワークエイリアスでサービス名を解決できるが、
    /// Apple Container にはエイリアスがなく、コンテナ名からしか引けない。
    /// 後者はこの一覧を使って `MINATO_HOST_<SERVICE>` を注入し、
    /// 相手のホスト名をアプリに伝える。
    pub peers: Vec<String>,
}

impl ServiceSpec {
    pub fn name(&self) -> &str {
        &self.key.service
    }

    /// 環境変数を `KEY=VALUE` の並びにする。
    pub fn env_pairs(&self) -> Vec<String> {
        self.env.iter().map(|(k, v)| format!("{k}={v}")).collect()
    }
}

/// worktree のソースをコンテナに見せる指定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceMount {
    pub host: PathBuf,
    pub target: String,
}

/// 永続領域のマウント指定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VolumeMount {
    /// runtime が管理する名前付き領域。
    Named {
        name: String,
        target: String,
        read_only: bool,
    },
    /// ホストのパスを直接マウントする。
    Bind {
        source: PathBuf,
        target: String,
        read_only: bool,
    },
}

impl VolumeMount {
    /// `minato.toml` の `volumes` に書かれた 1 行を解釈する。
    ///
    /// - `pgdata:/var/lib/postgresql/data` — 名前付き領域
    /// - `./seed:/seed` / `/abs/path:/data` — ホストパス（`base` からの相対）
    /// - 末尾に `:ro` を付けると読み取り専用
    pub fn parse(spec: &str, base: &std::path::Path) -> Result<Self, String> {
        let parts: Vec<&str> = spec.split(':').collect();

        let (source, target, read_only) = match parts.as_slice() {
            [source, target] => (*source, *target, false),
            [source, target, "ro"] => (*source, *target, true),
            [source, target, "rw"] => (*source, *target, false),
            _ => {
                return Err(format!(
                    "volumes の書式が不正です: `{spec}`。\
                     `名前:/コンテナ側パス` または `./ホスト側:/コンテナ側[:ro]` で指定してください"
                ));
            }
        };

        if source.is_empty() || target.is_empty() {
            return Err(format!("volumes の書式が不正です: `{spec}`"));
        }

        if !target.starts_with('/') {
            return Err(format!(
                "volumes のコンテナ側パスは絶対パスにしてください: `{spec}`"
            ));
        }

        // `/` や `.` で始まればホストパス、それ以外は名前付き領域。
        if source.starts_with('/') || source.starts_with('.') || source.starts_with('~') {
            let path = if source.starts_with('/') {
                PathBuf::from(source)
            } else if let Some(rest) = source.strip_prefix("~/") {
                match dirs_home() {
                    Some(home) => home.join(rest),
                    None => return Err(format!("ホームディレクトリを解決できません: `{spec}`")),
                }
            } else {
                base.join(source)
            };

            Ok(Self::Bind {
                source: path,
                target: target.to_string(),
                read_only,
            })
        } else {
            Ok(Self::Named {
                name: source.to_string(),
                target: target.to_string(),
                read_only,
            })
        }
    }

    pub fn target(&self) -> &str {
        match self {
            Self::Named { target, .. } | Self::Bind { target, .. } => target,
        }
    }

    pub fn read_only(&self) -> bool {
        match self {
            Self::Named { read_only, .. } | Self::Bind { read_only, .. } => *read_only,
        }
    }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// 起動したサービス。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningService {
    pub key: ServiceKey,
    pub container_id: String,

    /// プロキシが転送する先。
    ///
    /// Docker ではホストにフォワードされた `127.0.0.1:<動的ポート>`、
    /// Apple Container ではコンテナ自身の `192.168.64.x:<ポート>` になる。
    /// **この差を吸収するのがこの型の存在意義**で、プロキシは実装を知らずに済む。
    pub endpoint: Option<SocketAddr>,
}

/// runtime に問い合わせた現在の状態。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceStatus {
    pub key: ServiceKey,
    pub state: ServiceState,
    pub container_id: Option<String>,
    pub image: Option<String>,
    pub endpoint: Option<SocketAddr>,
    pub port: Option<u16>,
    pub scope: ServiceScope,
}

impl ServiceStatus {
    /// コンテナが存在しないときの状態。
    pub fn stopped(key: ServiceKey, scope: ServiceScope) -> Self {
        Self {
            key,
            state: ServiceState::Stopped,
            container_id: None,
            image: None,
            endpoint: None,
            port: None,
            scope,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn shared_workspace_cannot_collide_with_real_labels() {
        // 実在の workspace 名は必ず DNS ラベルなので、`_` 始まりとは衝突しない。
        assert!(!minato_core::naming::is_valid_label(SHARED_WORKSPACE));
        assert!(WorkspaceKey::shared("myapp").is_shared());
        assert!(!WorkspaceKey::new("myapp", "feat-1").is_shared());
    }

    #[test]
    fn service_key_displays_as_path() {
        let key = WorkspaceKey::new("myapp", "feat-1").service("web");
        assert_eq!(key.to_string(), "myapp/feat-1/web");
    }

    #[test]
    fn parses_named_volume() {
        let base = Path::new("/repo");
        let volume = VolumeMount::parse("pgdata:/var/lib/postgresql/data", base).expect("valid");

        assert_eq!(
            volume,
            VolumeMount::Named {
                name: "pgdata".into(),
                target: "/var/lib/postgresql/data".into(),
                read_only: false,
            }
        );
    }

    #[test]
    fn parses_relative_bind_against_base() {
        let volume = VolumeMount::parse("./seed:/seed", Path::new("/repo")).expect("valid");
        assert_eq!(
            volume,
            VolumeMount::Bind {
                source: PathBuf::from("/repo/./seed"),
                target: "/seed".into(),
                read_only: false,
            }
        );
    }

    #[test]
    fn parses_absolute_bind() {
        let volume = VolumeMount::parse("/data:/data:ro", Path::new("/repo")).expect("valid");
        assert!(volume.read_only());
        assert!(matches!(volume, VolumeMount::Bind { .. }));
    }

    #[test]
    fn parses_read_write_suffix() {
        let volume = VolumeMount::parse("pgdata:/data:rw", Path::new("/repo")).expect("valid");
        assert!(!volume.read_only());
    }

    #[test]
    fn rejects_relative_container_path() {
        let err = VolumeMount::parse("pgdata:data", Path::new("/repo")).unwrap_err();
        assert!(err.contains("絶対パス"), "got: {err}");
    }

    #[test]
    fn rejects_malformed_volume() {
        for spec in ["pgdata", "", "a:b:c:d", ":/data", "pgdata:"] {
            assert!(
                VolumeMount::parse(spec, Path::new("/repo")).is_err(),
                "`{spec}` は拒否されるべき"
            );
        }
    }

    #[test]
    fn env_pairs_are_sorted_and_formatted() {
        let spec = ServiceSpec {
            key: WorkspaceKey::new("myapp", "feat-1").service("web"),
            attached_to: WorkspaceKey::new("myapp", "feat-1"),
            image: "node:22".into(),
            command: None,
            workdir: "/workspace".into(),
            env: BTreeMap::from([
                ("PORT".to_string(), "3000".to_string()),
                ("NODE_ENV".to_string(), "development".to_string()),
            ]),
            port: Some(3000),
            health: None,
            scope: ServiceScope::Workspace,
            volumes: vec![],
            source_mount: None,
            peers: vec![],
        };

        // BTreeMap なので順序が安定する。コンテナの再作成判定に効く。
        assert_eq!(spec.env_pairs(), vec!["NODE_ENV=development", "PORT=3000"]);
    }
}
