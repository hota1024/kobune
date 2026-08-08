//! The spec handed to a runtime, written in vocabulary no implementation
//! owns.
//!
//! Letting Docker-specific concepts in — compose, network drivers — would
//! bend the Apple Container and Firecracker implementations out of shape.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use minato_core::{HealthCheck, ServiceScope, ServiceState};

/// The notional workspace that `scope = "project"` services belong to.
///
/// A shared instance belongs to no particular worktree, so its labels use
/// this reserved name. A leading `_` is invalid in a DNS label, so it can
/// never collide with a real workspace name.
pub const SHARED_WORKSPACE: &str = "_shared";

/// Identifies one workspace.
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

    /// The key that `scope = "project"` services belong to.
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

/// Identifies one service instance.
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

/// The spec for handling one workspace as a whole.
#[derive(Debug, Clone)]
pub struct WorkspaceSpec {
    pub key: WorkspaceKey,
    /// The worktree path, mounted into the containers.
    pub worktree_path: PathBuf,
    pub services: Vec<ServiceSpec>,
}

impl WorkspaceSpec {
    pub fn service(&self, name: &str) -> Option<&ServiceSpec> {
        self.services.iter().find(|s| s.key.service == name)
    }
}

/// What it takes to start one service.
#[derive(Debug, Clone)]
pub struct ServiceSpec {
    /// For `scope = "project"`, `key.workspace` is
    /// [`WorkspaceKey::shared`].
    pub key: ServiceKey,

    /// The workspace that wants this service.
    ///
    /// Even a shared service has to join the caller's workspace network.
    pub attached_to: WorkspaceKey,

    pub image: String,
    pub command: Option<Vec<String>>,
    pub workdir: String,
    pub env: BTreeMap<String, String>,

    /// The port listened on inside the container.
    pub port: Option<u16>,

    /// How to decide the service is ready to serve.
    ///
    /// Unset means "can a TCP connection be made".
    pub health: Option<HealthCheck>,

    pub scope: ServiceScope,
    pub volumes: Vec<VolumeMount>,

    /// How to mount the worktree source. `None` for a shared service.
    pub source_mount: Option<SourceMount>,

    /// The other services running in the same workspace.
    ///
    /// Docker resolves service names through network aliases. Apple
    /// Container has no aliases and can only resolve container names, so
    /// it uses this list to inject `MINATO_HOST_<SERVICE>` and tell the app
    /// what to call its neighbours.
    pub peers: Vec<String>,
}

impl ServiceSpec {
    pub fn name(&self) -> &str {
        &self.key.service
    }

    /// The environment as a list of `KEY=VALUE`.
    pub fn env_pairs(&self) -> Vec<String> {
        self.env.iter().map(|(k, v)| format!("{k}={v}")).collect()
    }
}

/// How the worktree source is exposed to a container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceMount {
    pub host: PathBuf,
    pub target: String,
}

/// A persistent mount.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VolumeMount {
    /// Named storage the runtime manages.
    Named {
        name: String,
        target: String,
        read_only: bool,
    },
    /// A host path, mounted directly.
    Bind {
        source: PathBuf,
        target: String,
        read_only: bool,
    },
}

impl VolumeMount {
    /// Parses one line of `volumes` from `minato.toml`.
    ///
    /// - `pgdata:/var/lib/postgresql/data` — named storage
    /// - `./seed:/seed`, `/abs/path:/data` — a host path, relative to `base`
    /// - a trailing `:ro` makes it read-only
    pub fn parse(spec: &str, base: &std::path::Path) -> Result<Self, String> {
        let parts: Vec<&str> = spec.split(':').collect();

        let (source, target, read_only) = match parts.as_slice() {
            [source, target] => (*source, *target, false),
            [source, target, "ro"] => (*source, *target, true),
            [source, target, "rw"] => (*source, *target, false),
            _ => {
                return Err(format!(
                    "malformed volumes entry: `{spec}`. \
                     Write `name:/container/path` or `./host:/container[:ro]`"
                ));
            }
        };

        if source.is_empty() || target.is_empty() {
            return Err(format!("malformed volumes entry: `{spec}`"));
        }

        if !target.starts_with('/') {
            return Err(format!(
                "the container path in volumes has to be absolute: `{spec}`"
            ));
        }

        // A leading `/` or `.` means a host path; anything else is named
        // storage.
        if source.starts_with('/') || source.starts_with('.') || source.starts_with('~') {
            let path = if source.starts_with('/') {
                PathBuf::from(source)
            } else if let Some(rest) = source.strip_prefix("~/") {
                match dirs_home() {
                    Some(home) => home.join(rest),
                    None => return Err(format!("cannot resolve the home directory: `{spec}`")),
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

/// A service that has been started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningService {
    pub key: ServiceKey,
    pub container_id: String,

    /// Where the proxy forwards to.
    ///
    /// Under Docker, a forwarded `127.0.0.1:<dynamic port>`; under Apple
    /// Container, the container's own `192.168.64.x:<port>`. **Absorbing
    /// that difference is what this type is for** — the proxy never has to
    /// know which runtime it is talking to.
    pub endpoint: Option<SocketAddr>,
}

/// The current state, as the runtime reports it.
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
    /// The state when there is no container.
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
        // A real workspace name is always a DNS label, so nothing
        // starting with `_` can collide with one.
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
        assert!(err.contains("absolute"), "got: {err}");
    }

    #[test]
    fn rejects_malformed_volume() {
        for spec in ["pgdata", "", "a:b:c:d", ":/data", "pgdata:"] {
            assert!(
                VolumeMount::parse(spec, Path::new("/repo")).is_err(),
                "`{spec}` should be rejected"
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

        // A BTreeMap keeps the order stable, which is what makes the
        // "should this container be recreated" check work.
        assert_eq!(spec.env_pairs(), vec!["NODE_ENV=development", "PORT=3000"]);
    }
}
