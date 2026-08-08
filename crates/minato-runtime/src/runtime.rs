//! The interface every virtualisation backend shares.
//!
//! The [`RunningService::endpoint`] this trait returns is "where the proxy
//! forwards to". Whether that is a forwarded host port or the container's
//! own IP is up to the implementation; neither the proxy nor the
//! supervisor knows the difference.

use async_trait::async_trait;
use futures::stream::BoxStream;
use minato_api::OutputStream;

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

/// The result of running a command inside a container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutcome {
    /// The exit code of the command that ran.
    ///
    /// **Passed straight back to the caller.** An agent has to be able to
    /// judge `minato exec web -- pnpm test` by its exit code alone.
    pub exit_code: i32,
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

    /// Runs a command inside the container.
    ///
    /// Output goes to `events`; the exit code comes back. No TTY is
    /// requested — for agent use, no interaction is the safer default.
    async fn exec(
        &self,
        key: &ServiceKey,
        command: &[String],
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
    pub fn volume(project: &str, name: &str) -> String {
        format!("minato-{project}-{name}")
    }

    /// Some implementations reject a leading `_` in a container name, so
    /// `_shared` loses it.
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
        assert_eq!(names::volume("myapp", "pgdata"), "minato-myapp-pgdata");
        assert_ne!(
            names::volume("myapp", "pgdata"),
            names::volume("other", "pgdata"),
            "a different project means different storage"
        );
    }
}
