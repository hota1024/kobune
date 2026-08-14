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
        /// Which service this describes, when it describes one.
        ///
        /// Without it two listings are structurally identical, so anything
        /// storing or comparing them cannot tell whose environment it kept.
        #[serde(default)]
        service: Option<String>,
    },
    /// The result of a command. Its output arrives as [`crate::Event::Output`].
    Exec {
        /// The exit code of the command that was run.
        ///
        /// The CLI passes it through as its own exit code, so an agent can
        /// judge `minato exec web -- pnpm test` by exit status alone.
        exit_code: i32,
    },
    /// The state of the Cloudflare Tunnel.
    Tunnel(TunnelInfo),

    /// What `Purge` found, or what it took down.
    Purge(PurgeReport),

    /// Operations with nothing to return (`rm` / `shutdown`).
    Empty,
}

/// Everything the daemon owns, listed so it can be taken down — or, after
/// it has been, what went.
///
/// Structured rather than counted: "3 containers" is not something a person
/// can check, and an agent deciding whether to go ahead needs the names.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PurgeReport {
    /// Whether this is what would happen, or what did.
    #[serde(default)]
    pub dry_run: bool,

    pub projects: Vec<PurgeProject>,

    /// Worktrees Minato created and is **leaving in place**.
    ///
    /// Removing them is `minato rm`, one at a time and with a `--force`
    /// that means something. An uninstaller that deleted a checkout would
    /// take uncommitted work with it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub worktrees: Vec<PathBuf>,

    /// The Cloudflare Tunnel, when one was ever set up.
    ///
    /// Stopped and forgotten locally. The named tunnel and its DNS records
    /// live in the user's Cloudflare account, and deleting things from
    /// someone's account is not something an uninstaller should do behind
    /// their back — so what is left is reported, with the command that
    /// removes it, the same way `tunnel enable` reports its setup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tunnel: Option<TunnelLeftover>,

    /// The storage Minato made, and is taking with it.
    ///
    /// **Listed, not swept along quietly.** A project volume outlives the
    /// worktrees that used it — that is what it is for — so what is in one
    /// is often the only copy of a development database somebody has been
    /// filling for months. Uninstall asks before it removes anything, and a
    /// question is only worth asking if what is going is on the list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<PurgeVolume>,

    /// Storage that could not be listed, or would not go, and why.
    ///
    /// **Structured rather than logged past.** A runtime that cannot be
    /// asked answers the same way as one holding nothing — an empty list —
    /// and the difference is the whole question: an uninstall that could
    /// not reach Docker would otherwise print a plan with no storage in
    /// it, remove everything else, and exit 0, leaving volumes behind
    /// under names only Minato knew.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub storage_left: Vec<PurgeStorageFailure>,

    /// Projects whose containers could not be taken down, and why.
    ///
    /// A runtime that is not running is the usual cause. These keep their
    /// entry in the state file so a later run can finish the job — which
    /// is the only reason a caller can be told to try again rather than
    /// left with containers nothing remembers the name of.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stranded: Vec<PurgeFailure>,
}

/// What stays in the Cloudflare account after an uninstall.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunnelLeftover {
    /// The zone the hostnames lived under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// What to run to remove it, if that is wanted.
    pub commands: Vec<String>,
}

/// A project that survived the purge, and what stopped it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PurgeFailure {
    pub project: String,
    pub reason: String,
}

/// One volume, named the way its runtime names it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PurgeVolume {
    /// The project whose storage it is, empty when it belongs to none.
    pub project: String,

    /// What the runtime calls it: the name `docker volume ls` prints, or
    /// the directory Apple Container's bind mount lives in.
    ///
    /// The real name rather than the one written in `minato.toml`, so that
    /// somebody who wants to keep one can find it before saying yes.
    pub name: String,
}

/// Storage an uninstall did not manage to account for.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PurgeStorageFailure {
    /// The runtime that could not be asked, or the volume that would not
    /// go — whichever it was, named the way a person could go and look.
    pub what: String,
    pub reason: String,
}

impl PurgeReport {
    /// Whether the daemon has anything of its own left.
    ///
    /// **Storage it could not ask about counts as something left.** Not
    /// knowing and having nothing are different answers, and only one of
    /// them makes "nothing of Minato's was found" true.
    pub fn is_empty(&self) -> bool {
        self.volumes.is_empty()
            && self.storage_left.is_empty()
            && self.projects.iter().all(|project| {
                project
                    .workspaces
                    .iter()
                    .all(|workspace| workspace.services.is_empty())
            })
    }

    pub fn service_count(&self) -> usize {
        self.projects
            .iter()
            .flat_map(|project| &project.workspaces)
            .map(|workspace| workspace.services.len())
            .sum()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PurgeProject {
    pub name: String,
    pub workspaces: Vec<PurgeWorkspace>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PurgeWorkspace {
    pub label: String,
    /// The containers behind it, by service name.
    pub services: Vec<String>,
}

/// Where the tunnel stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunnelInfo {
    pub state: TunnelState,

    /// The zone the hostnames live under. `None` before setup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,

    /// The wildcard record routed for the zone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<String>,

    /// What the user has to run before the daemon can continue.
    ///
    /// Empty once setup is done. `cloudflared tunnel login` opens a
    /// browser and waits, so it is reported rather than run — the same
    /// reason `minato setup` runs nothing where there is no terminal to
    /// answer at.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup: Vec<String>,

    /// Whether the tunnel is unauthenticated.
    ///
    /// True means the environment is on the public internet with no
    /// Cloudflare Access policy that Minato knows of.
    #[serde(default)]
    pub public: bool,
}

impl TunnelInfo {
    pub fn disabled() -> Self {
        Self {
            state: TunnelState::Disabled,
            domain: None,
            record: None,
            setup: Vec::new(),
            public: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TunnelState {
    /// Never set up, or turned off.
    Disabled,
    /// `cloudflared` is not installed.
    NotInstalled,
    /// Installed, but `cloudflared tunnel login` has not been run.
    NeedsLogin,
    /// Configured but not currently up.
    Stopped,
    /// Carrying traffic.
    Running,
}

impl TunnelState {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::NotInstalled => "not installed",
            Self::NeedsLogin => "needs login",
            Self::Stopped => "stopped",
            Self::Running => "running",
        }
    }

    /// Whether tunnel URLs are worth showing.
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running)
    }
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
    /// Why this value is shown as written, when it is.
    ///
    /// **Per value rather than per listing**, so that one bad reference
    /// does not put every other value under suspicion. `None` means the
    /// value is what the container would be given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unsettled: Option<Unsettled>,
}

/// Why a value could not be expanded.
///
/// Structured, not a sentence: the CLI and the GUI say it their own way
/// (`docs/DESIGN.md` §3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unsettled {
    /// The name it refers to. Absent for a loop, which has no single one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    pub reason: UnsettledReason,
}

/// What stood in the way of expanding a value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsettledReason {
    /// Nothing in this environment sets the name.
    Undefined,
    /// Only a listing about one service holds it — `MINATO_SERVICE`, or a
    /// service's own `env`. The environment itself is fine; this listing
    /// is the one that cannot settle it.
    OnlyWithService {
        /// A service that does have it.
        service: String,
    },
    /// Injected only while the proxy is listening, and only for a service
    /// that publishes a URL.
    NeedsProxy,
    /// A secret resolves in memory at start-up, so it cannot be built into
    /// another value.
    Secret,
    /// The references form a loop.
    Cycle {
        /// The loop, in the order it was walked.
        chain: Vec<String>,
    },
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
    /// `stopped` | `starting` | `ready` | `idle` | `failed` | `unknown`,
    /// as a plain string.
    pub state: ServiceState,
    /// Why, when `state` is `failed`. Absent otherwise.
    ///
    /// **Beside the state rather than inside it.** That is what lets
    /// `state` be a string an agent can compare — see
    /// [`minato_core::ServiceState`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
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
            reason: None,
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
    fn a_purge_report_roundtrips() {
        let report = PurgeReport {
            dry_run: true,
            projects: vec![PurgeProject {
                name: "myapp".into(),
                workspaces: vec![PurgeWorkspace {
                    label: "feat-1".into(),
                    services: vec!["web".into()],
                }],
            }],
            worktrees: vec![PathBuf::from("/repo/myapp.wt/feat-1")],
            ..PurgeReport::default()
        };

        let json = serde_json::to_string(&Response::Purge(report.clone())).expect("serializes");
        let back: Response = serde_json::from_str(&json).expect("deserializes");

        match back {
            Response::Purge(back) => assert_eq!(back, report),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn a_report_with_no_containers_is_empty_even_with_workspaces() {
        // A registered workspace whose containers are already gone is
        // nothing left to take down, and `uninstall` should not claim it
        // has work to do.
        let report = PurgeReport {
            dry_run: false,
            projects: vec![PurgeProject {
                name: "myapp".into(),
                workspaces: vec![PurgeWorkspace {
                    label: "feat-1".into(),
                    services: Vec::new(),
                }],
            }],
            worktrees: Vec::new(),
            ..PurgeReport::default()
        };

        assert!(report.is_empty());
        assert_eq!(report.service_count(), 0);
    }

    #[test]
    fn storage_left_over_is_not_an_empty_report() {
        // The state a machine is usually in by the time anyone uninstalls:
        // every worktree `minato rm`ed, so no containers left, and the
        // project volumes they shared still there. `uninstall` asks this
        // before deciding it has nothing to do — and there is a database
        // to remove.
        let report = PurgeReport {
            volumes: vec![PurgeVolume {
                project: "myapp".into(),
                name: "minato-myapp-pgdata".into(),
            }],
            ..PurgeReport::default()
        };

        assert!(!report.is_empty());
        assert_eq!(report.service_count(), 0);
    }

    #[test]
    fn storage_that_could_not_be_asked_about_is_not_an_empty_report() {
        // Docker not running, and every worktree already `minato rm`ed. An
        // empty listing and a listing that failed are the same shape and
        // opposite answers: reporting nothing here is how an uninstall
        // says "there was no storage" about volumes it never saw.
        let report = PurgeReport {
            storage_left: vec![PurgeStorageFailure {
                what: "docker".into(),
                reason: "its storage could not be listed: connection refused".into(),
            }],
            ..PurgeReport::default()
        };

        assert!(!report.is_empty());
    }

    #[test]
    fn a_report_counts_every_service_across_projects() {
        let report = PurgeReport {
            dry_run: false,
            projects: vec![
                PurgeProject {
                    name: "a".into(),
                    workspaces: vec![PurgeWorkspace {
                        label: "main".into(),
                        services: vec!["web".into(), "db".into()],
                    }],
                },
                PurgeProject {
                    name: "b".into(),
                    workspaces: vec![PurgeWorkspace {
                        label: "main".into(),
                        services: vec!["api".into()],
                    }],
                },
            ],
            worktrees: Vec::new(),
            ..PurgeReport::default()
        };

        assert!(!report.is_empty());
        assert_eq!(report.service_count(), 3);
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
        assert!(
            !json.contains("tunnel_url"),
            "unused fields stay off the wire"
        );
        // Check for the key, not the value of `"scope":"workspace"`.
        assert!(!json.contains(r#""workspace":"#), "got: {json}");
    }
}
