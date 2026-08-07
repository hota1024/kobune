//! The final response from the daemon to a client.
//!
//! No pre-formatted, human-facing strings belong here. Presentation is the
//! CLI's and the GUI's job (`docs/DESIGN.md` §3).

use std::path::PathBuf;

use minato_core::{ServiceScope, ServiceState};
use serde::{Deserialize, Serialize};

use crate::diagnostics::Diagnostics;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Pong(Pong),
    /// Operations returning several workspaces (`ls`).
    Workspaces {
        workspaces: Vec<WorkspaceInfo>,
    },
    /// Operations returning one workspace (`new` / `up` / `down` / `status`).
    Workspace {
        workspace: WorkspaceInfo,
    },
    /// Diagnostics (`doctor`).
    Diagnostics(Diagnostics),
    /// A listing of environment variables.
    Env {
        entries: Vec<EnvInfo>,
    },
    /// The result of a command. Its output arrives as [`crate::Event::Output`].
    Exec {
        /// The exit code of the command that was run.
        ///
        /// The CLI passes it through as its own exit code, so an agent can
        /// judge `minato exec web -- pnpm test` by exit status alone.
        exit_code: i32,
    },
    /// Operations with nothing to return (`rm` / `shutdown`).
    Empty,
}

/// A single environment variable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvInfo {
    pub key: String,
    /// The value for display. Masked by default.
    pub value: String,
    /// Which layer defined it.
    pub scope: minato_core::EnvScope,
    /// Whether this is a secret reference.
    #[serde(default)]
    pub secret: bool,
    /// A description of the reference. Never the value itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pong {
    /// The daemon's version.
    pub version: String,
    /// The protocol version it speaks.
    pub protocol: u32,
    /// The default runtime implementation.
    pub runtime: String,
    pub uptime_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub project: String,
    /// The workspace label used in URLs. `None` for the main worktree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    pub branch: String,
    pub path: PathBuf,
    pub is_main: bool,
    pub services: Vec<ServiceInfo>,
}

impl WorkspaceInfo {
    /// The display name. `(main)` for the main worktree.
    pub fn display_name(&self) -> &str {
        self.workspace.as_deref().unwrap_or("(main)")
    }

    pub fn service(&self, name: &str) -> Option<&ServiceInfo> {
        self.services.iter().find(|s| s.name == name)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceInfo {
    pub name: String,
    pub state: ServiceState,
    pub scope: ServiceScope,

    /// The issued URL. Present once the proxy is listening.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    /// The URL via Cloudflare Tunnel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tunnel_url: Option<String>,

    /// The address reachable directly from the host (`127.0.0.1:49312`).
    ///
    /// Without a proxy this is the only way in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,

    /// The port inside the container.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_id: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

impl ServiceInfo {
    /// What a client should show as the way in.
    ///
    /// The URL when one has been issued, otherwise the raw address.
    pub fn access(&self) -> Option<String> {
        self.url
            .clone()
            .or_else(|| self.endpoint.as_ref().map(|e| format!("http://{e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(name: &str) -> ServiceInfo {
        ServiceInfo {
            name: name.into(),
            state: ServiceState::Ready,
            scope: ServiceScope::Workspace,
            url: None,
            tunnel_url: None,
            endpoint: None,
            port: None,
            container_id: None,
            image: None,
        }
    }

    #[test]
    fn access_prefers_url_over_endpoint() {
        let mut svc = service("web");
        svc.endpoint = Some("127.0.0.1:49312".into());
        assert_eq!(svc.access().as_deref(), Some("http://127.0.0.1:49312"));

        svc.url = Some("https://web.feat-1.myapp.localhost".into());
        assert_eq!(
            svc.access().as_deref(),
            Some("https://web.feat-1.myapp.localhost")
        );
    }

    #[test]
    fn access_is_none_without_any_address() {
        assert_eq!(service("db").access(), None);
    }

    #[test]
    fn main_workspace_displays_as_main() {
        let info = WorkspaceInfo {
            project: "myapp".into(),
            workspace: None,
            branch: "main".into(),
            path: PathBuf::from("/repo"),
            is_main: true,
            services: vec![service("web")],
        };

        assert_eq!(info.display_name(), "(main)");
        assert!(info.service("web").is_some());
        assert!(info.service("nope").is_none());
    }

    #[test]
    fn omits_empty_optionals_on_the_wire() {
        let info = WorkspaceInfo {
            project: "myapp".into(),
            workspace: None,
            branch: "main".into(),
            path: PathBuf::from("/repo"),
            is_main: true,
            services: vec![service("web")],
        };

        let json = serde_json::to_string(&info).expect("serializes");
        assert!(!json.contains("tunnel_url"), "unused fields stay off the wire");
        // Check for the key, not the value of `"scope":"workspace"`.
        assert!(!json.contains(r#""workspace":"#), "got: {json}");
    }
}
