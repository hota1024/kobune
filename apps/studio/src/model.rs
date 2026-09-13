//! What the browser is sent.
//!
//! Deliberately thin. `kobune-api`'s types already carry everything the
//! screen needs and they are already `Serialize`, so these wrap rather
//! than restate them — a field added to `ServiceInfo` reaches the
//! dashboard without being copied out by hand here first.
//!
//! The exceptions are the two counts. "3/3" is on every sidebar row, and
//! whether a service counts as running is
//! [`ServiceState::is_running`](kobune_core::ServiceState::is_running) —
//! `Starting | Ready | Idle`. Deriving that in TypeScript would put a
//! second copy of the predicate somewhere nothing would notice it drifting.

use kobune_api::{Diagnostics, EnvInfo, ServiceInfo, TunnelInfo, WorkspaceInfo};
use serde::Serialize;

/// One poll's worth of screen.
#[derive(Debug, Clone, Serialize)]
pub struct StateView {
    pub daemon: DaemonView,
    /// The project the working directory resolved to.
    pub project: Option<String>,
    pub tunnel: TunnelView,
    pub workspaces: Vec<WorkspaceView>,
}

/// The daemon behind the top bar's dot.
#[derive(Debug, Clone, Serialize)]
pub struct DaemonView {
    pub version: String,
    pub protocol: u32,
    pub runtime: String,
    pub uptime_secs: u64,
}

impl From<kobune_api::Pong> for DaemonView {
    fn from(pong: kobune_api::Pong) -> Self {
        Self {
            version: pong.version,
            protocol: pong.protocol,
            runtime: pong.runtime,
            uptime_secs: pong.uptime_secs,
        }
    }
}

/// The tunnel chip.
///
/// An `Option` would conflate "not asked" with "off", and the chip has to
/// tell them apart: the first is a dashboard that could not reach the
/// daemon, the second is a setting.
#[derive(Debug, Clone, Serialize)]
pub struct TunnelView {
    pub state: String,
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    pub public: bool,
}

impl From<TunnelInfo> for TunnelView {
    fn from(info: TunnelInfo) -> Self {
        Self {
            state: info.state.label().to_string(),
            running: info.state.is_running(),
            domain: info.domain,
            public: info.public,
        }
    }
}

impl TunnelView {
    /// What the chip shows when the daemon could not be asked.
    pub fn unknown() -> Self {
        Self {
            state: "unknown".into(),
            running: false,
            domain: None,
            public: false,
        }
    }
}

/// A sidebar row, a tab, and — when it is the open one — the detail pane.
#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceView {
    /// `(main)` for the main worktree, which is what its row says.
    pub label: String,
    /// `None` for the main worktree — the hostname carries no label there.
    pub workspace: Option<String>,
    pub branch: String,
    pub path: String,
    pub is_main: bool,
    pub running: usize,
    pub total: usize,
    pub services: Vec<ServiceInfo>,
}

impl From<WorkspaceInfo> for WorkspaceView {
    fn from(info: WorkspaceInfo) -> Self {
        let running = info
            .services
            .iter()
            .filter(|service| service.state.is_running())
            .count();

        Self {
            label: info.display_name().to_string(),
            running,
            total: info.services.len(),
            workspace: info.workspace,
            branch: info.branch,
            path: info.path.display().to_string(),
            is_main: info.is_main,
            services: info.services,
        }
    }
}

/// The `doctor` overlay.
#[derive(Debug, Clone, Serialize)]
pub struct DoctorView {
    #[serde(flatten)]
    pub diagnostics: Diagnostics,
}

/// The `env` overlay.
#[derive(Debug, Clone, Serialize)]
pub struct EnvView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    pub entries: Vec<EnvInfo>,
}

/// One line in the log pane.
///
/// `service` is optional because [`kobune_api::Event::Output`] leaves it
/// so: a build writes lines that belong to no single service.
#[derive(Debug, Clone, Serialize)]
pub struct LogLine {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    /// `stdout`, `stderr`, or `log` for the daemon's own commentary.
    pub stream: String,
    pub text: String,
}

/// What an action reports when it is over.
#[derive(Debug, Clone, Serialize)]
pub struct ActionResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}
