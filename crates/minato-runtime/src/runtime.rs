//! The interface every virtualisation backend shares.
//!
//! The [`RunningService::endpoint`] this trait returns is "where the proxy
//! forwards to". Whether that is a forwarded host port or the container's
//! own IP is up to the implementation; neither the proxy nor the
//! supervisor knows the difference.

use std::pin::Pin;

use async_trait::async_trait;
use futures::stream::BoxStream;
use minato_api::{OutputStream, Window};
use tokio::io::AsyncWrite;

use crate::error::Result;
use crate::event::EventSink;
use crate::spec::{
    RunningService, ServiceKey, ServiceSpec, ServiceStatus, WorkspaceKey, WorkspaceSpec,
};

/// How to read logs.
#[derive(Debug, Clone, Default)]
pub struct LogOptions {
    /// Keep waiting for new lines.
    pub follow: bool,
    /// How many lines to take from the end. `None` means all of them.
    pub tail: Option<usize>,
}

/// One line of log output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    pub stream: OutputStream,
    pub line: String,
}

/// A live terminal on a running service.
///
/// Only a service started with `tty` has one. The two halves are
/// independent: output keeps arriving while nobody is typing, and what is
/// typed is echoed by the terminal inside the container rather than here.
pub struct Attachment {
    /// What the container's terminal produces, in the order it produced it.
    ///
    /// Chunks, not lines. A full-screen program's output is not made of
    /// lines, and cutting it into them is not something that can be undone
    /// further along.
    pub output: BoxStream<'static, Vec<u8>>,

    /// Where keystrokes go.
    pub input: Pin<Box<dyn AsyncWrite + Send>>,

    /// The size this terminal is stuck at, when it cannot be resized.
    ///
    /// `None` means [`Runtime::resize`] works. Apple Container reads the
    /// size once, when the service starts, so what it says here is what a
    /// full-screen program will see however large the window really is —
    /// and the caller passes that on rather than letting someone wonder
    /// why the display is the wrong shape.
    pub fixed_size: Option<Window>,
}

impl std::fmt::Debug for Attachment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Attachment")
    }
}

/// How to run a command inside a container.
#[derive(Debug, Clone, Default)]
pub struct ExecOptions {
    /// Where to run it. The service's own `workdir` when left out.
    pub workdir: Option<String>,
}

/// The result of running a command inside a container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutcome {
    /// The exit code of the command that ran.
    ///
    /// **Passed straight back to the caller.** An agent has to be able to
    /// judge `minato exec web -- pnpm test` by its exit code alone.
    pub exit_code: i32,
}

/// What makes a one-off debugging container different from the real one.
pub(crate) struct Throwaway<'a> {
    /// Its own name, so it cannot collide with the service's container.
    pub(crate) name: String,
    /// What to run instead of the service's `command`.
    pub(crate) command: &'a [String],
    /// Where to run it, when somewhere other than the service's `workdir`.
    pub(crate) workdir: Option<&'a str>,
}

impl<'a> Throwaway<'a> {
    /// A name that will not collide with the real container or with
    /// another throwaway running beside it.
    ///
    /// Prefixed distinctly from `minato-`, so anything left behind by a
    /// daemon that died mid-command reads as debris rather than as a
    /// service.
    pub(crate) fn new(spec: &ServiceSpec, command: &'a [String], workdir: Option<&'a str>) -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or_default();

        Self {
            name: format!(
                "minato-tmp-{}-{}-{stamp}",
                spec.key.workspace.project, spec.key.service
            ),
            command,
            workdir,
        }
    }
}

/// What a runtime is. Shown by `minato doctor` and `minato ping`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeInfo {
    pub id: String,
    pub version: String,
    /// Whether it can create its own networks.
    ///
    /// Apple Container cannot before macOS 26, leaving nothing to do but
    /// share the default network.
    pub supports_custom_networks: bool,
}

/// A virtualisation backend.
#[async_trait]
pub trait Runtime: Send + Sync {
    /// The identifier written under `[runtime] default` in `minato.toml`.
    fn id(&self) -> &'static str;

    /// Checks that it can be reached and that its version is usable.
    async fn probe(&self) -> Result<RuntimeInfo>;

    /// Groundwork for one workspace: networks, volumes and images.
    ///
    /// `rebuild` forces a build even when an image with the same tag is
    /// already present. Left false, an existing tag is taken to mean the
    /// image is current, which is what keeps waking a stopped service from
    /// running a build.
    async fn prepare(&self, spec: &WorkspaceSpec, rebuild: bool, events: &EventSink) -> Result<()>;

    /// Starts a service. Already running: returns as-is, does nothing.
    async fn start(&self, spec: &ServiceSpec, events: &EventSink) -> Result<RunningService>;

    /// Stops a service, keeping the container so the next start is fast.
    async fn stop(&self, key: &ServiceKey, events: &EventSink) -> Result<()>;

    /// Removes a service's container.
    async fn remove(&self, key: &ServiceKey, events: &EventSink) -> Result<()>;

    /// Clears away everything belonging to a workspace, network included.
    ///
    /// Shared services (`scope = "project"`) are left alone: other
    /// workspaces are using them.
    async fn destroy_workspace(&self, key: &WorkspaceKey, events: &EventSink) -> Result<()>;

    /// The current state of one service.
    ///
    /// A container that exited abnormally is [`ServiceState::Failed`], not
    /// [`ServiceState::Stopped`] — see the contract on those variants. A
    /// backend whose API cannot tell the two apart says so where it maps
    /// its states, rather than leaving the next reader to wonder.
    async fn inspect(&self, key: &ServiceKey) -> Result<ServiceStatus>;

    /// Reads logs.
    ///
    /// Without this an agent has nothing to fall back on but `docker logs`.
    async fn logs(
        &self,
        key: &ServiceKey,
        options: LogOptions,
    ) -> Result<BoxStream<'static, LogLine>>;

    /// Opens the service's terminal, in both directions.
    ///
    /// Only for a service whose spec asked for `tty`; without one there is
    /// no terminal to open and this fails. That is a condition the caller
    /// is expected to have checked, not one to surprise a person with:
    /// `minato logs` reads the service's configuration first and falls
    /// back to plain log reading.
    ///
    /// Several attachments to one service are possible and all see the
    /// same terminal, exactly as two `docker attach`es do. Nothing here
    /// arbitrates between them.
    async fn attach(&self, key: &ServiceKey) -> Result<Attachment>;

    /// Tells the container's terminal how big the window is.
    ///
    /// A full-screen program asks its terminal for the size and draws to
    /// it. Without this it would draw to the 80×24 the runtime invented.
    async fn resize(&self, key: &ServiceKey, cols: u16, rows: u16) -> Result<()>;

    /// Runs a command inside the container.
    ///
    /// Output goes to `events`; the exit code comes back. No TTY is
    /// requested — for agent use, no interaction is the safer default.
    async fn exec(
        &self,
        key: &ServiceKey,
        command: &[String],
        options: &ExecOptions,
        events: &EventSink,
    ) -> Result<ExecOutcome>;

    /// Runs a command in a container made for the purpose, then removes it.
    ///
    /// **The service does not have to be running**, which is the whole
    /// point: a start-up script that fails leaves nothing to exec into, and
    /// that is when someone most wants to look around. The image, the
    /// environment and the volumes are the service's; the command is not.
    ///
    /// It publishes no ports and carries no Minato labels, so it cannot
    /// take the real container's ports, appear in `list_project`, or answer
    /// to the service's name on the network.
    async fn exec_fresh(
        &self,
        spec: &ServiceSpec,
        command: &[String],
        options: &ExecOptions,
        events: &EventSink,
    ) -> Result<ExecOutcome>;

    /// Every Minato-managed service in a project.
    ///
    /// This is how the daemon recovers its state after a restart. The
    /// runtime, not a state store, is the source of truth, so this listing
    /// has to be trustworthy.
    async fn list_project(&self, project: &str) -> Result<Vec<ServiceStatus>>;
}

/// The label keys put on containers.
///
/// The same keys across every runtime. These labels are all the daemon has
/// to tell its own containers apart from everyone else's.
pub mod labels {
    /// Marks a container as Minato's. The value is `"1"`.
    pub const MANAGED: &str = "dev.minato.managed";
    pub const PROJECT: &str = "dev.minato.project";
    /// The workspace label. `_shared` for a shared service.
    pub const WORKSPACE: &str = "dev.minato.workspace";
    pub const SERVICE: &str = "dev.minato.service";
    /// Either `workspace` or `project`.
    pub const SCOPE: &str = "dev.minato.scope";
    /// The port listened on inside the container.
    pub const PORT: &str = "dev.minato.port";

    /// Marks a one-off container from `minato exec --fresh`.
    ///
    /// **It carries no `SERVICE` label**, which is what keeps it out of
    /// `list_project` and therefore out of `minato status` and the routing
    /// table. It carries the rest so that one left behind by a daemon that
    /// died mid-command is still findable — an unlabelled container is
    /// invisible to every Minato command for ever.
    pub const THROWAWAY: &str = "dev.minato.throwaway";

    /// What a built image was built from.
    ///
    /// Put on the image, not the container. A build is skipped when the
    /// image already carries the current value, so this is what decides
    /// whether `up` rebuilds.
    pub const BUILD_FINGERPRINT: &str = "dev.minato.build";

    pub const MANAGED_VALUE: &str = "1";
}

/// The shared naming rules every runtime implementation follows.
pub mod names {
    use crate::spec::{ServiceKey, WorkspaceKey};

    /// The container name.
    ///
    /// Shaped so it means something to a person reading `docker ps` or
    /// `container ls`.
    pub fn container(key: &ServiceKey) -> String {
        format!(
            "minato-{}-{}-{}",
            key.workspace.project,
            sanitize_segment(&key.workspace.workspace),
            key.service
        )
    }

    /// The network name for one workspace.
    pub fn network(key: &WorkspaceKey) -> String {
        format!(
            "minato-{}-{}",
            key.project,
            sanitize_segment(&key.workspace)
        )
    }

    /// The real name of a named volume. Never collides across projects.
    ///
    /// A workspace-scoped one carries the worktree too, which is what keeps
    /// two branches from sharing storage whose shape they disagree about.
    ///
    /// **The worktree is joined with `.`, not `-`.** Projects, worktrees and
    /// volume names are all DNS labels, so a hyphen is legal inside every
    /// one of them and joining with it leaves the two forms sharing a
    /// namespace: under worktree `feat-1`, a project volume named
    /// `feat-1-cache` and a workspace volume named `cache` would be the same
    /// storage. A `.` cannot occur in a label, so the two can never meet.
    pub fn volume(key: &WorkspaceKey, name: &str, scope: crate::spec::VolumeScope) -> String {
        match scope {
            crate::spec::VolumeScope::Project => format!("minato-{}-{name}", key.project),
            crate::spec::VolumeScope::Workspace => {
                debug_assert!(
                    !key.is_shared(),
                    "a shared service has no worktree to scope storage to; \
                     the configuration is meant to refuse this"
                );

                format!(
                    "minato-{}-{}.{name}",
                    key.project,
                    sanitize_segment(&key.workspace)
                )
            }
        }
    }

    /// Some implementations reject a leading `_` in a container name, so
    /// `_shared` loses it.
    pub(crate) fn sanitize_segment(segment: &str) -> String {
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
            "dropped because some implementations reject `_`: {name}"
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
        use crate::spec::VolumeScope;

        let one = WorkspaceKey::new("myapp", "feat-1");
        let two = WorkspaceKey::new("myapp", "feat-2");
        let other = WorkspaceKey::new("other", "feat-1");

        assert_eq!(
            names::volume(&one, "pgdata", VolumeScope::Project),
            "minato-myapp-pgdata"
        );
        assert_eq!(
            names::volume(&one, "pgdata", VolumeScope::Project),
            names::volume(&two, "pgdata", VolumeScope::Project),
            "the project scope is what every worktree shares"
        );
        assert_ne!(
            names::volume(&one, "pgdata", VolumeScope::Project),
            names::volume(&other, "pgdata", VolumeScope::Project),
            "a different project means different storage"
        );
    }

    #[test]
    fn a_workspace_volume_is_not_shared_between_worktrees() {
        // The whole point: two branches whose lockfiles disagree must not
        // be handed the same node_modules.
        use crate::spec::VolumeScope;

        let one = WorkspaceKey::new("myapp", "feat-1");
        let two = WorkspaceKey::new("myapp", "feat-2");

        assert_eq!(
            names::volume(&one, "node-modules", VolumeScope::Workspace),
            "minato-myapp-feat-1.node-modules"
        );
        assert_ne!(
            names::volume(&one, "node-modules", VolumeScope::Workspace),
            names::volume(&two, "node-modules", VolumeScope::Workspace)
        );
    }

    #[test]
    fn a_workspace_volume_does_not_collide_with_a_project_one() {
        use crate::spec::VolumeScope;

        let key = WorkspaceKey::new("myapp", "feat-1");

        assert_ne!(
            names::volume(&key, "cache", VolumeScope::Workspace),
            names::volume(&key, "cache", VolumeScope::Project)
        );
    }

    #[test]
    fn a_hyphenated_project_volume_cannot_be_a_workspace_one() {
        // Joined with `-`, worktree `feat-1` + name `cache` and the project
        // volume `feat-1-cache` are the same string: the shared and the
        // per-worktree volume become one, silently, under the scope whose
        // entire job is keeping them apart. Volume names are labels, and a
        // label cannot contain `.`, so `.` is what makes that impossible.
        use crate::spec::VolumeScope;

        let key = WorkspaceKey::new("myapp", "feat-1");

        assert_ne!(
            names::volume(&key, "cache", VolumeScope::Workspace),
            names::volume(&key, "feat-1-cache", VolumeScope::Project)
        );
    }

    #[test]
    fn two_worktrees_cannot_be_talked_into_the_same_volume() {
        // Branch `feat` with `1-cache`, branch `feat-1` with `cache`.
        use crate::spec::VolumeScope;

        let short = WorkspaceKey::new("myapp", "feat");
        let long = WorkspaceKey::new("myapp", "feat-1");

        assert_ne!(
            names::volume(&short, "1-cache", VolumeScope::Workspace),
            names::volume(&long, "cache", VolumeScope::Workspace)
        );
    }
}
