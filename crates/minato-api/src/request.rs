//! Requests from a client to the daemon.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Identifies what an operation acts on.
///
/// The daemon does not know the caller's working directory, so the client
/// always supplies it. The git repository and `minato.toml` are resolved
/// from `cwd`; when `workspace` is omitted, the worktree containing `cwd`
/// is the target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Target {
    pub cwd: PathBuf,

    /// An explicit workspace label. Inferred from `cwd` when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

impl Target {
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            workspace: None,
        }
    }

    pub fn workspace(mut self, workspace: Option<String>) -> Self {
        self.workspace = workspace;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Connectivity check and version handshake.
    Ping,

    /// Shuts the daemon down.
    Shutdown,

    /// Diagnoses the environment, reporting what the daemon can see.
    Doctor,

    /// Lists workspaces.
    Ls {
        target: Target,
        /// Return every project, not just the current one.
        #[serde(default)]
        all_projects: bool,
    },

    /// Creates a worktree and prepares its environment.
    New {
        target: Target,
        /// The branch to check out. Created from `base` if absent.
        branch: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base: Option<String>,
        /// Where to create the worktree. Derived by convention if omitted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<PathBuf>,
        /// Whether to start the services afterwards.
        #[serde(default = "yes")]
        start: bool,
    },

    /// Destroys a worktree and its environment.
    Rm {
        target: Target,
        /// Delete even with uncommitted changes.
        #[serde(default)]
        force: bool,
    },

    /// Starts services.
    Up {
        target: Target,
        /// The services to act on. Empty means all of them.
        #[serde(default)]
        services: Vec<String>,
    },

    /// Stops services.
    Down {
        target: Target,
        #[serde(default)]
        services: Vec<String>,
        /// Stop every workspace in the project.
        #[serde(default)]
        all: bool,
    },

    /// The current state of a workspace.
    Status { target: Target },

    /// Reads logs. Output arrives as [`crate::Event::Output`].
    Logs {
        target: Target,
        /// The services to read. All of them when omitted.
        #[serde(default)]
        services: Vec<String>,
        /// Keep waiting for new lines.
        #[serde(default)]
        follow: bool,
        /// How many lines to take from the end.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tail: Option<usize>,
    },

    /// Runs a command inside a container.
    Exec {
        target: Target,
        service: String,
        command: Vec<String>,
    },

    /// Lists environment variables.
    EnvList {
        target: Target,
        /// Show values in the clear. Masked by default.
        #[serde(default)]
        reveal: bool,
    },

    /// Sets an environment variable.
    EnvSet {
        target: Target,
        scope: minato_core::EnvScope,
        key: String,
        value: String,
    },

    /// Removes an environment variable.
    EnvUnset {
        target: Target,
        scope: minato_core::EnvScope,
        key: String,
    },

    /// Sets up the Cloudflare Tunnel and starts it.
    TunnelEnable {
        target: Target,
        /// The zone the hostnames live under. Reuses the configured one
        /// when omitted, so re-enabling does not mean naming it again.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        domain: Option<String>,
        /// Acknowledges that the environment goes on the public internet
        /// with no Cloudflare Access policy in front of it.
        ///
        /// Not a convenience flag. Minato cannot apply an Access policy —
        /// that needs the Cloudflare API, not the CLI — so it cannot
        /// promise one is there. Exposing a development environment
        /// unauthenticated is an accident unless it was asked for
        /// (`docs/DESIGN.md` §9).
        #[serde(default)]
        public: bool,
    },

    /// Stops the tunnel. The named tunnel itself is left in place.
    TunnelDisable { target: Target },

    /// Reports where the tunnel setup stands.
    TunnelStatus { target: Target },
}

fn yes() -> bool {
    true
}

impl Request {
    /// Whether this is a long-running operation that emits progress.
    ///
    /// Clients show a progress indicator when this is true.
    pub fn is_long_running(&self) -> bool {
        matches!(
            self,
            Self::New { .. }
                | Self::Up { .. }
                | Self::Down { .. }
                | Self::Rm { .. }
                | Self::Logs { .. }
                | Self::Exec { .. }
                | Self::TunnelEnable { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_operations() {
        let json = serde_json::to_string(&Request::Ping).expect("serializes");
        assert_eq!(json, r#"{"op":"ping"}"#);
    }

    #[test]
    fn roundtrips_new_request() {
        let request = Request::New {
            target: Target::new(PathBuf::from("/repo")),
            branch: "feature/one".into(),
            base: None,
            path: None,
            start: true,
        };

        let json = serde_json::to_string(&request).expect("serializes");
        let back: Request = serde_json::from_str(&json).expect("deserializes");

        match back {
            Request::New { branch, start, .. } => {
                assert_eq!(branch, "feature/one");
                assert!(start);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn start_defaults_to_true() {
        // A default of false would make `minato new` skip starting up.
        let request: Request =
            serde_json::from_str(r#"{"op":"new","target":{"cwd":"/repo"},"branch":"x"}"#)
                .expect("deserializes");

        match request {
            Request::New { start, .. } => assert!(start, "start defaults to true"),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn classifies_long_running_operations() {
        let target = Target::new(PathBuf::from("/repo"));
        assert!(!Request::Ping.is_long_running());
        assert!(
            !Request::Status {
                target: target.clone()
            }
            .is_long_running()
        );
        assert!(
            Request::Up {
                target,
                services: vec![]
            }
            .is_long_running()
        );
    }
}
