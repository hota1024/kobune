//! Owning the Cloudflare Tunnel process.
//!
//! The tunnel is machine-wide, not per-workspace: one named tunnel, one
//! wildcard ingress rule, and the proxy sorts out what each hostname
//! means. So this holds a single process and the daemon starts and stops
//! it, rather than anything being tied to a workspace's lifecycle.
//!
//! Enabling is idempotent. `cloudflared tunnel create` and `route dns` are
//! both run on every enable and on every daemon start, and "it already
//! exists" reads as success — the alternative is a flag in the state file
//! that can disagree with what Cloudflare actually has.

use std::sync::Arc;

use minato_api::{TunnelInfo, TunnelState};
use minato_core::TunnelRecord;
use minato_tunnel::{Readiness, TunnelProcess, TunnelSettings};
use tokio::sync::Mutex;

/// The tunnel, as the daemon holds it.
#[derive(Default)]
pub struct TunnelHandle {
    running: Mutex<Option<TunnelProcess>>,
    /// The domain being served right now, or `None` when nothing is up.
    ///
    /// Separate from the process so it can be read synchronously.
    /// [`Self::domain`] is consulted while building every status response
    /// and every routing table, which are not async paths.
    active: std::sync::RwLock<Option<String>>,
}

impl TunnelHandle {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Whether traffic is flowing.
    ///
    /// Asks the child rather than trusting the flag: cloudflared exiting
    /// on its own — a revoked credential, a deleted tunnel — would
    /// otherwise leave `status` claiming it is up while every tunnel URL
    /// 502s. A child found dead clears [`Self::domain`], so the URLs stop
    /// being advertised too.
    pub async fn is_running(&self) -> bool {
        let mut guard = self.running.lock().await;

        let alive = match guard.as_mut() {
            Some(process) => process.is_running(),
            None => false,
        };

        if !alive {
            guard.take();
            self.set_domain(None);
        }

        alive
    }

    /// The domain being served, if any.
    pub fn domain(&self) -> Option<String> {
        self.active
            .read()
            .ok()
            .and_then(|domain| domain.as_ref().cloned())
    }

    fn set_domain(&self, domain: Option<String>) {
        if let Ok(mut guard) = self.active.write() {
            *guard = domain;
        }
    }

    /// Starts the tunnel, replacing one that is already up.
    ///
    /// Replacing rather than refusing means `tunnel enable --domain` with
    /// a new domain does the obvious thing.
    pub async fn start(
        &self,
        settings: TunnelSettings,
        projects: Vec<String>,
    ) -> Result<(), minato_tunnel::TunnelError> {
        let mut guard = self.running.lock().await;

        if let Some(existing) = guard.take() {
            existing.stop().await;
        }

        let domain = settings.domain.clone();
        *guard = Some(TunnelProcess::start(settings, &projects).await?);
        self.set_domain(Some(domain));

        Ok(())
    }

    /// Stops the tunnel. Doing nothing when it is already down.
    pub async fn stop(&self) {
        let mut guard = self.running.lock().await;
        if let Some(process) = guard.take() {
            process.stop().await;
        }
        self.set_domain(None);
    }
}

/// Builds the settings for a record.
///
/// `local_port` is the proxy's plain-HTTP port. Without one the tunnel has
/// nowhere to send traffic, so enabling is refused rather than started
/// into a dead end.
pub fn settings_for(
    record: &TunnelRecord,
    dir: std::path::PathBuf,
    local_port: u16,
) -> TunnelSettings {
    TunnelSettings::new(&record.domain, dir, local_port).with_name(&record.name)
}

/// Reports where setup stands, without running anything.
pub async fn info(
    record: Option<&TunnelRecord>,
    handle: &TunnelHandle,
    settings: Option<&TunnelSettings>,
    project: &str,
) -> TunnelInfo {
    let Some(record) = record else {
        return TunnelInfo::disabled();
    };

    // Turned off outranks everything. Reporting "needs login" about a
    // feature somebody deliberately switched off is noise, and `doctor`
    // would carry it on every run.
    let state = if !record.enabled {
        TunnelState::Disabled
    } else {
        match settings.map(minato_tunnel::readiness) {
            Some(Readiness::NotInstalled) => TunnelState::NotInstalled,
            Some(Readiness::NeedsLogin) => TunnelState::NeedsLogin,
            _ if handle.is_running().await => TunnelState::Running,
            _ => TunnelState::Stopped,
        }
    };

    let setup = match (state, settings) {
        (TunnelState::NotInstalled, _) => {
            vec!["brew install cloudflared".to_string()]
        }
        (TunnelState::NeedsLogin, Some(settings)) => settings.setup_commands(),
        _ => Vec::new(),
    };

    TunnelInfo {
        state,
        domain: Some(record.domain.clone()),
        record: settings.map(|settings| settings.dns_record(project)),
        setup,
        // Minato cannot apply an Access policy through the CLI, so it
        // cannot claim one is in place. Anything it did enable was
        // acknowledged with `--public`.
        public: record.enabled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn record(enabled: bool) -> TunnelRecord {
        TunnelRecord {
            name: "minato".into(),
            domain: "example.com".into(),
            enabled,
            routed: BTreeSet::new(),
        }
    }

    #[tokio::test]
    async fn no_record_reads_as_disabled() {
        let handle = TunnelHandle::default();
        let info = info(None, &handle, None, "myapp").await;

        assert_eq!(info.state, TunnelState::Disabled);
        assert!(info.domain.is_none());
    }

    #[tokio::test]
    async fn a_missing_cloudflared_outranks_being_enabled() {
        // Enabled in the state file but nothing to run it with. Reporting
        // "stopped" would send someone looking for the wrong problem.
        let handle = TunnelHandle::default();
        let settings = TunnelSettings::new("example.com", "/tmp", 80)
            .with_program("minato-definitely-not-a-real-program");

        let info = info(Some(&record(true)), &handle, Some(&settings), "myapp").await;

        assert_eq!(info.state, TunnelState::NotInstalled);
        assert!(!info.setup.is_empty(), "it says what to install");
    }

    #[tokio::test]
    async fn a_configured_but_stopped_tunnel_keeps_its_domain() {
        // `disable` clears the flag and keeps the record, so re-enabling
        // does not mean naming the domain again.
        let handle = TunnelHandle::default();
        let settings = TunnelSettings::new("example.com", "/tmp", 80).with_program("/bin/sh");

        let info = info(Some(&record(false)), &handle, Some(&settings), "myapp").await;

        assert_eq!(info.state, TunnelState::Disabled);
        assert_eq!(info.domain.as_deref(), Some("example.com"));
    }

    #[tokio::test]
    async fn status_names_the_record_this_project_needs() {
        // "Nothing resolves" usually means the DNS route is missing, and
        // the answer is the record to look for.
        let handle = TunnelHandle::default();
        let settings = TunnelSettings::new("example.com", "/tmp", 80).with_program("/bin/sh");

        let info = info(Some(&record(true)), &handle, Some(&settings), "myapp").await;

        assert_eq!(info.record.as_deref(), Some("*.myapp.example.com"));
    }

    #[tokio::test]
    async fn nothing_running_reads_as_not_running() {
        let handle = TunnelHandle::default();

        assert!(!handle.is_running().await);
        assert!(handle.domain().is_none(), "no domain to advertise");
    }
}
