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
    BuildSpec, RunningService, ServiceKey, ServiceSpec, ServiceStatus, SourceMount, VolumeMount,
    VolumeScope, WorkspaceKey, WorkspaceSpec,
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

/// What to do about a runtime that cannot be reached.
///
/// Runtime-specific, because the answers have nothing in common: one is a
/// desktop application to launch, the other a service to register. Being
/// told to start Docker Desktop when the project runs on Apple Container
/// is worse than being told nothing.
pub fn start_hint(id: &str) -> &'static str {
    match id {
        "apple" | "apple-container" | "container" => {
            "start the Apple Container service with `container system start`"
        }
        _ => "start one of Docker Desktop, OrbStack or colima",
    }
}

/// A human-readable name for a runtime identifier.
pub fn display_name(id: &str) -> &'static str {
    match id {
        "apple" | "apple-container" | "container" => "Apple Container",
        "docker" => "Docker",
        _ => "container runtime",
    }
}

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
    fn no_available_runtime_is_unknown() {
        // `AVAILABLE_RUNTIMES` is what `doctor` iterates and what the
        // "unknown runtime" message lists. An entry `create` does not
        // recognise would be advertised and then refused.
        //
        // Only `Unsupported` is checked, not success: `create` reaches for
        // a Docker socket, so demanding success would make this a test of
        // whether Docker happens to be running. It passed on a laptop and
        // failed in a container, which is the definition of the wrong
        // assertion.
        for id in AVAILABLE_RUNTIMES {
            if let Err(RuntimeError::Unsupported(message)) = create(id) {
                panic!("{id} is advertised but not recognised: {message}");
            }
        }
    }

    #[test]
    fn each_runtime_suggests_its_own_way_back() {
        // Telling an Apple Container user to start Docker Desktop sends
        // them somewhere that cannot help.
        assert!(start_hint("apple").contains("container system start"));
        assert!(start_hint("docker").contains("Docker Desktop"));
        assert_ne!(start_hint("apple"), start_hint("docker"));
    }

    #[test]
    fn accepts_apple_container_aliases() {
        for id in ["apple", "apple-container", "container"] {
            let runtime = create(id).expect("creates");
            assert_eq!(runtime.id(), "apple");
        }
    }
}
