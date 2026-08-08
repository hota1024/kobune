//! Cloudflare Tunnel, so an environment is reachable from outside.
//!
//! One named tunnel per machine carries every project. Its ingress sends
//! everything to the local proxy and the proxy routes on Host, so
//! workspaces come and go without the tunnel's configuration or DNS being
//! touched (`docs/DESIGN.md` §9).
//!
//! **Nothing interactive runs from here.** `cloudflared tunnel login`
//! opens a browser and waits, which would hang an agent exactly the way an
//! unattended `sudo` does. Login is reported as a step for the user to
//! take; everything after it — creating the tunnel, routing DNS, running
//! it — the daemon does itself.

pub mod config;
pub mod process;

use std::path::{Path, PathBuf};

pub use config::{IngressConfig, render_config};
pub use process::TunnelProcess;

/// The default named tunnel.
///
/// One per machine. Reusing the name means `tunnel enable` is idempotent
/// across projects.
pub const DEFAULT_TUNNEL_NAME: &str = "minato";

/// The CLI this drives.
pub const PROGRAM: &str = "cloudflared";

/// Overrides which binary is run.
///
/// For a cloudflared that is installed somewhere off `PATH`, and for
/// exercising the daemon's tunnel path without a Cloudflare account.
pub const PROGRAM_ENV: &str = "MINATO_CLOUDFLARED";

/// The command to run, honouring [`PROGRAM_ENV`].
pub fn program() -> String {
    program_from(std::env::var(PROGRAM_ENV).ok().as_deref())
}

/// Picks the command from an override, if there is a usable one.
///
/// Split out from [`program`] so it can be tested without setting a process
/// variable. A test that did would race every other test in the crate:
/// they all build settings, and building settings reads this. That race
/// existed, passed on macOS, and failed under Linux's scheduling.
fn program_from(override_value: Option<&str>) -> String {
    override_value
        .filter(|value| !value.is_empty())
        .unwrap_or(PROGRAM)
        .to_string()
}

/// Where `cloudflared tunnel login` leaves its certificate.
///
/// Its presence is what tells us whether login has happened; there is no
/// other way to ask without making a network call.
pub fn login_cert_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| Path::new(&home).join(".cloudflared").join("cert.pem"))
}

#[derive(Debug, thiserror::Error)]
pub enum TunnelError {
    #[error("no `{0}` command found")]
    NotInstalled(String),

    #[error("cloudflared is not logged in")]
    NotLoggedIn,

    #[error("{operation} failed: {message}")]
    Failed { operation: String, message: String },

    #[error("cannot write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl TunnelError {
    pub fn failed(operation: impl Into<String>, message: impl std::fmt::Display) -> Self {
        Self::Failed {
            operation: operation.into(),
            message: message.to_string(),
        }
    }
}

pub type Result<T, E = TunnelError> = std::result::Result<T, E>;

/// How far along the setup is.
///
/// Everything before [`Self::Ready`] needs the user, so each step is
/// reported rather than attempted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    /// `cloudflared` is not on the PATH.
    NotInstalled,
    /// Installed, but `cloudflared tunnel login` has not been run.
    NeedsLogin,
    /// Logged in. The daemon can take it from here.
    Ready,
}

impl Readiness {
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }
}

/// What is needed to run a tunnel.
#[derive(Debug, Clone)]
pub struct TunnelSettings {
    /// The named tunnel.
    pub name: String,
    /// The zone the hostnames live under (`example.com`).
    pub domain: String,
    /// Where the generated configuration and logs go.
    pub dir: PathBuf,
    /// The local proxy's plain-HTTP port.
    ///
    /// The tunnel terminates TLS at Cloudflare's edge, so the hop to the
    /// proxy is plain HTTP over loopback and never leaves the machine.
    /// Going to the HTTPS port instead would mean cloudflared verifying
    /// the local CA, which it has no reason to trust.
    pub local_port: u16,
    /// The command to run. Overridable for tests.
    pub program: String,
}

impl TunnelSettings {
    pub fn new(domain: impl Into<String>, dir: impl Into<PathBuf>, local_port: u16) -> Self {
        Self {
            name: DEFAULT_TUNNEL_NAME.to_string(),
            domain: domain.into(),
            dir: dir.into(),
            local_port,
            program: program(),
        }
    }

    pub fn with_program(mut self, program: impl Into<String>) -> Self {
        self.program = program.into();
        self
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Where the generated ingress configuration is written.
    pub fn config_path(&self) -> PathBuf {
        self.dir.join("config.yml")
    }

    /// The wildcard record that routes a project's hostnames here.
    ///
    /// One per project rather than one per workspace, which is what keeps
    /// DNS still while worktrees come and go.
    pub fn dns_record(&self, project: &str) -> String {
        format!("*.{project}.{}", self.domain)
    }

    /// The steps the user has to take before the daemon can continue.
    pub fn setup_commands(&self) -> Vec<String> {
        vec![format!("{} tunnel login", self.program)]
    }
}

/// Whether the machine is set up far enough for the daemon to proceed.
pub fn readiness(settings: &TunnelSettings) -> Readiness {
    if which(&settings.program).is_none() {
        return Readiness::NotInstalled;
    }

    match login_cert_path() {
        Some(path) if path.is_file() => Readiness::Ready,
        // Without a home directory there is nowhere for the certificate to
        // be, so treat it as absent rather than guess.
        _ => Readiness::NeedsLogin,
    }
}

/// Looks a program up on `PATH`.
///
/// `Command::spawn` would answer this too, but only by running the thing.
/// Readiness is reported before anything is run, so the lookup happens
/// here.
fn which(program: &str) -> Option<PathBuf> {
    if program.contains('/') {
        let path = PathBuf::from(program);
        return path.is_file().then_some(path);
    }

    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths).find_map(|dir| {
        let candidate = dir.join(program);
        candidate.is_file().then_some(candidate)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> TunnelSettings {
        TunnelSettings::new("example.com", "/tmp/minato-tunnel", 80)
    }

    #[test]
    fn dns_record_is_one_wildcard_per_project() {
        // Per workspace it would mean a DNS write every time a worktree
        // appeared, which is what the wildcard exists to avoid.
        assert_eq!(settings().dns_record("myapp"), "*.myapp.example.com");
    }

    #[test]
    fn setup_only_asks_for_the_interactive_step() {
        // Everything else the daemon does itself. Asking the user to run
        // more than they must is how setup instructions go stale.
        let commands = settings().setup_commands();

        assert_eq!(commands.len(), 1);
        assert!(commands[0].contains("tunnel login"), "got: {commands:?}");
    }

    #[test]
    fn missing_program_is_reported_before_anything_runs() {
        let settings = settings().with_program("minato-definitely-not-a-real-program");
        assert_eq!(readiness(&settings), Readiness::NotInstalled);
    }

    #[test]
    fn an_absolute_program_path_is_used_as_given() {
        // Tests point `program` at a stub script, which is not on PATH.
        let settings = settings().with_program("/bin/sh");
        assert_ne!(readiness(&settings), Readiness::NotInstalled);
    }

    #[test]
    fn the_program_can_be_pointed_elsewhere() {
        // For a cloudflared installed off PATH, and the hook the daemon's
        // own tunnel path is exercised through.
        assert_eq!(
            program_from(Some("/opt/custom/cloudflared")),
            "/opt/custom/cloudflared"
        );
        assert_eq!(program_from(None), PROGRAM);

        // An exported-but-empty variable is how a shell says "unset".
        assert_eq!(program_from(Some("")), PROGRAM);
    }

    #[test]
    fn config_lives_beside_the_other_generated_files() {
        assert_eq!(
            settings().config_path(),
            PathBuf::from("/tmp/minato-tunnel/config.yml")
        );
    }
}
