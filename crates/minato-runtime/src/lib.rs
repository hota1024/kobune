//! The virtualisation-backend abstraction, and its implementations.
//!
//! The `endpoint` a `Runtime` returns is where the proxy forwards to; each
//! implementation decides whether that is a forwarded host port or the
//! container's own IP. That single choice absorbs the structural
//! difference between Docker and Apple Container.

pub mod apple;
pub mod docker;
pub mod error;
pub mod event;
pub mod health;
pub mod runtime;
pub mod spec;

pub use apple::AppleContainerRuntime;
pub use docker::DockerRuntime;
pub use error::{Result, RuntimeError};
pub use event::EventSink;
pub use health::{DEFAULT_READINESS_TIMEOUT, await_service, probe, wait_until_ready};
pub use runtime::{ExecOutcome, LogLine, LogOptions, Runtime, RuntimeInfo, labels, names};
pub use spec::{
    RunningService, ServiceKey, ServiceSpec, ServiceStatus, SourceMount, VolumeMount, WorkspaceKey,
    WorkspaceSpec,
};

/// Builds a runtime implementation from its identifier.
///
/// Nothing is connected to, so success here does not mean the runtime is
/// usable. Callers check reachability with [`Runtime::probe`].
pub fn create(id: &str) -> Result<Box<dyn Runtime>> {
    match id {
        "docker" => Ok(Box::new(DockerRuntime::connect()?)),
        "apple" | "apple-container" | "container" => Ok(Box::new(AppleContainerRuntime::new())),
        other => Err(RuntimeError::Unsupported(format!(
            "no such runtime `{other}`. Use `docker` or `apple`"
        ))),
    }
}

/// The runtime identifiers that are supported.
pub const AVAILABLE_RUNTIMES: &[&str] = &["docker", "apple"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_runtime_with_a_useful_message() {
        // `Box<dyn Runtime>` is not Debug, so unwrap_err is out.
        let message = match create("podman") {
            Ok(runtime) => panic!("expected no support, got {}", runtime.id()),
            Err(err) => err.to_string(),
        };

        assert!(message.contains("podman"), "what went wrong: {message}");
        assert!(message.contains("docker"), "what to use instead: {message}");
    }

    #[test]
    fn accepts_apple_container_aliases() {
        for id in ["apple", "apple-container", "container"] {
            let runtime = create(id).expect("creates");
            assert_eq!(runtime.id(), "apple");
        }
    }
}
