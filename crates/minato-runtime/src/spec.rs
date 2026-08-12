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

    /// The image to run. When [`Self::build`] is set this is the tag the
    /// build produces, not something to pull.
    pub image: String,

    /// How to build the image, when the service builds rather than pulls.
    pub build: Option<BuildSpec>,

    pub command: Option<Vec<String>>,
    pub workdir: String,
    pub env: BTreeMap<String, String>,

    /// Run the process on a terminal, with its stdin left open.
    ///
    /// Set from `tty` in `minato.toml`. It is what lets `minato logs`
    /// attach both ways, and what makes a program that checks for a
    /// terminal — turborepo, most test runners — draw in colour.
    pub tty: bool,

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

    /// The hostnames from `MINATO_URL_<SERVICE>`, to be pointed at the
    /// gateway from inside the container.
    ///
    /// **A URL that only works in the browser is half a URL.** The same
    /// `https://api.myapp.localhost` is what the frontend's server side
    /// calls, and without this it resolves on the host alone: inside a
    /// container the name is NXDOMAIN, and the app has to fall back to
    /// `http://api:8080` — a different Host and Origin than the browser
    /// sends, which is where cookie domains and CORS start disagreeing
    /// between the two halves of one app.
    ///
    /// Every runtime maps these to wherever it reaches the host; a
    /// `depends_on` is not required, since it is the gateway that answers
    /// and it is always up. Empty when no proxy is running, because then
    /// there is nothing behind the names — see `Gateway::url_for`.
    pub gateway_hosts: Vec<String>,
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

/// How to build an image from a Dockerfile.
///
/// The tag and the fingerprint are settled by the daemon rather than the
/// runtime, so both backends produce the same name for the same service
/// and agree on when a rebuild is needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildSpec {
    /// The build context, as an absolute path.
    pub context: PathBuf,

    /// The Dockerfile, as an absolute path.
    ///
    /// Usually `context/Dockerfile`, but `dockerfile` in `minato.toml` can
    /// point elsewhere — one context, several images is a common layout.
    pub dockerfile: PathBuf,

    /// The tag the build produces.
    pub tag: String,

    /// What the image was built from.
    ///
    /// Stored as a label on the result. A build is skipped when the image
    /// already carries this value, which is what keeps `up` from rebuilding
    /// on every call and, more importantly, keeps waking a stopped service
    /// fast. See [`crate::labels::BUILD_FINGERPRINT`].
    pub fingerprint: String,

    /// `--build-arg` values, sorted so the fingerprint is stable.
    pub args: BTreeMap<String, String>,
}

/// How the worktree source is exposed to a container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceMount {
    pub host: PathBuf,
    pub target: String,
}

/// How far a named volume is shared.
///
/// **Project is the default, and stays the default.** A package cache is
/// what named storage is usually for, and sharing it is the point. Changing
/// the default would also rename every existing volume, which does not lose
/// the data but does hide it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VolumeScope {
    /// One volume for the project, mounted by every worktree.
    #[default]
    Project,
    /// One per worktree.
    ///
    /// For anything a branch changes the shape of. `node_modules` against a
    /// lockfile that differs per branch is the case that bites: shared, the
    /// two worktrees overwrite each other, and it reads as a broken install
    /// rather than as shared state.
    Workspace,
}

/// One piece of storage Minato is holding, as its runtime knows it.
///
/// What [`Runtime::managed_volumes`](crate::Runtime::managed_volumes)
/// finds and [`Runtime::remove_managed_volume`](crate::Runtime::remove_managed_volume)
/// takes back. The two are a pair on purpose: `id` is whatever the runtime
/// that listed it calls the thing — a Docker volume name, a directory on
/// Apple Container — so nothing above has to know which it is looking at.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ManagedVolume {
    /// The project whose storage it is.
    ///
    /// Empty when the volume carries no project of its own. It is still
    /// Minato's, and still goes; there is simply nothing to group it under.
    pub project: String,

    /// What the runtime calls it.
    pub id: String,
}

/// A persistent mount.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VolumeMount {
    /// Named storage the runtime manages.
    Named {
        name: String,
        target: String,
        read_only: bool,
        scope: VolumeScope,
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
    /// - `pgdata:/var/lib/postgresql/data` — named storage, shared by the
    ///   project's worktrees
    /// - `node-modules@workspace:/workspace/node_modules` — one per worktree
    /// - `./seed:/seed`, `/abs/path:/data` — a host path, relative to `base`
    /// - a trailing `:ro` makes it read-only
    ///
    /// `@workspace` goes on the name rather than in a third field: `:ro`
    /// already owns the position after the target, and the two are separate
    /// choices that have to be able to appear together.
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
            let (name, scope) = split_scope(source, spec)?;

            Ok(Self::Named {
                name,
                target: target.to_string(),
                read_only,
                scope,
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

/// Splits `node-modules@workspace` into its name and its scope.
///
/// An unknown suffix is refused rather than folded into the name. A typo
/// like `@worktree` would otherwise make a volume called
/// `node-modules@worktree` shared across every worktree — the mistake this
/// syntax exists to prevent, arrived at by making it.
fn split_scope(source: &str, spec: &str) -> Result<(String, VolumeScope), String> {
    let Some((name, suffix)) = source.split_once('@') else {
        return Ok((validate_name(source, spec)?, VolumeScope::Project));
    };

    let name = validate_name(name, spec)?;

    let scope = match suffix {
        "workspace" => VolumeScope::Workspace,
        "project" => VolumeScope::Project,
        other => {
            return Err(format!(
                "`@{other}` is not a volume scope in `{spec}`. \
                 Use `@workspace` for one per worktree, or leave it off to \
                 share it across the project"
            ));
        }
    };

    Ok((name, scope))
}

/// Checks a named volume's name.
///
/// **The same shape as a project or worktree name**, which is what lets
/// [`crate::names::volume`] argue that the two scopes cannot collide. It
/// also keeps the name usable as a directory: Apple Container has no named
/// volumes and joins this straight onto its storage root, where a `..`
/// would land outside it.
fn validate_name(name: &str, spec: &str) -> Result<String, String> {
    if name.is_empty() {
        return Err(format!("the volume name is empty: `{spec}`"));
    }

    if !minato_core::naming::is_valid_label(name) {
        return Err(format!(
            "`{name}` is not a usable volume name in `{spec}`. \
             Use lowercase letters, digits and hyphens"
        ));
    }

    Ok(name.to_string())
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
                scope: VolumeScope::Project,
            },
            "no suffix means the project shares it, as it always has"
        );
    }

    #[test]
    fn parses_a_workspace_scoped_volume() {
        let volume = VolumeMount::parse(
            "node-modules@workspace:/workspace/node_modules",
            Path::new("/repo"),
        )
        .expect("valid");

        assert_eq!(
            volume,
            VolumeMount::Named {
                name: "node-modules".into(),
                target: "/workspace/node_modules".into(),
                read_only: false,
                scope: VolumeScope::Workspace,
            }
        );
    }

    #[test]
    fn a_scope_composes_with_read_only() {
        // The two are separate choices, so neither may cost the other.
        let volume =
            VolumeMount::parse("certs@workspace:/certs:ro", Path::new("/repo")).expect("valid");

        assert_eq!(
            volume,
            VolumeMount::Named {
                name: "certs".into(),
                target: "/certs".into(),
                read_only: true,
                scope: VolumeScope::Workspace,
            }
        );
    }

    #[test]
    fn the_project_scope_can_be_written_out() {
        let volume = VolumeMount::parse("pgdata@project:/data", Path::new("/repo")).expect("valid");

        assert!(matches!(
            volume,
            VolumeMount::Named {
                scope: VolumeScope::Project,
                ..
            }
        ));
    }

    #[test]
    fn an_unknown_scope_is_refused_rather_than_kept_as_a_name() {
        // `node-modules@worktree` as a *name* would be shared across every
        // worktree — the exact mistake the suffix exists to prevent, made
        // by trying to prevent it.
        let err = VolumeMount::parse("node-modules@worktree:/x", Path::new("/repo")).unwrap_err();

        assert!(err.contains("@worktree"), "{err}");
        assert!(err.contains("@workspace"), "say what does work: {err}");
    }

    #[test]
    fn a_volume_name_has_to_be_a_label() {
        // It is joined into a Docker volume name and, on Apple Container,
        // straight onto a storage path — where a `/` would land outside
        // the root. Being a label is also what lets `names::volume` argue
        // the two scopes cannot collide.
        //
        // A leading `.` or `/` is a host path rather than a name, and those
        // are a different feature; `nested/name` is the case that reaches
        // this check.
        for bad in [
            "nested/name:/x",
            "Cache:/x",
            "with space:/x",
            "under_score:/x",
        ] {
            assert!(
                VolumeMount::parse(bad, Path::new("/repo")).is_err(),
                "accepted `{bad}`"
            );
        }
    }

    #[test]
    fn a_scope_on_a_host_path_is_not_a_scope() {
        // Bind mounts are the host's own directories; there is nothing to
        // namespace, and `@` is legal in a path.
        let volume = VolumeMount::parse("./seed@2:/seed", Path::new("/repo")).expect("valid");

        assert!(matches!(volume, VolumeMount::Bind { .. }));
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
            build: None,
            image: "node:22".into(),
            command: None,
            workdir: "/workspace".into(),
            env: BTreeMap::from([
                ("PORT".to_string(), "3000".to_string()),
                ("NODE_ENV".to_string(), "development".to_string()),
            ]),
            tty: false,
            port: Some(3000),
            health: None,
            scope: ServiceScope::Workspace,
            volumes: vec![],
            source_mount: None,
            peers: vec![],
            gateway_hosts: vec![],
        };

        // A BTreeMap keeps the order stable, which is what makes the
        // "should this container be recreated" check work.
        assert_eq!(spec.env_pairs(), vec!["NODE_ENV=development", "PORT=3000"]);
    }
}
