//! Owning the tunnel the daemon is running.
//!
//! The tunnel is machine-wide, not per-workspace: one named tunnel, one
//! ingress rule for the zone, and the proxy sorts out what each hostname
//! means. So this holds a single tunnel and the daemon starts and stops
//! it, rather than anything being tied to a workspace's lifecycle.
//!
//! **Which service carries it is not decided here.** What that service
//! needed to be told, what it left behind, and what it calls the state it
//! is in all belong to [`kobune_tunnel::TunnelProvider`]; this reports
//! what it is handed.
//!
//! Enabling is idempotent, on every provider: setup runs on every enable
//! and on every daemon start, and "it already exists" reads as success —
//! skipping the call on a stored flag instead would mean trusting the flag
//! over the service, which can disagree.

use std::sync::Arc;

use kobune_api::{TunnelAccess, TunnelInfo, TunnelState};
use kobune_core::TunnelRecord;
use kobune_tunnel::{
    Access, Hostnames, Readiness, RunningTunnel, StartOutcome, TunnelProvider, TunnelRequest,
};
use tokio::sync::Mutex;

/// A provider and what to ask it for.
///
/// The two are resolved together and neither is usable alone, so they
/// travel as a unit. Absent when the record names a provider this build
/// does not have, or when there is no proxy port for a tunnel to reach.
pub struct Configured {
    pub provider: Box<dyn TunnelProvider>,
    pub request: TunnelRequest,
}

impl Configured {
    fn readiness(&self) -> Readiness {
        self.provider.readiness(&self.request)
    }
}

/// The tunnel, as the daemon holds it.
#[derive(Default)]
pub struct TunnelHandle {
    running: Mutex<Option<Box<dyn RunningTunnel>>>,
    /// What arrives right now, or `None` when nothing is up.
    ///
    /// Separate from the tunnel so it can be read synchronously.
    /// [`Self::hostnames`] is consulted while building every status
    /// response and every routing table, which are not async paths.
    ///
    /// **A copy, not a borrow of the running tunnel.** For an assigned
    /// name that means the map as it was when the tunnel came up, which
    /// is also the only time it changes.
    active: std::sync::RwLock<Option<Hostnames>>,
}

impl TunnelHandle {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Whether traffic is flowing.
    ///
    /// Asks the tunnel rather than trusting the flag: one exiting on its
    /// own — a revoked credential, a deleted tunnel — would otherwise
    /// leave `status` claiming it is up while every tunnel URL 502s. One
    /// found dead clears [`Self::domain`], so the URLs stop being
    /// advertised too.
    pub async fn is_running(&self) -> bool {
        let mut guard = self.running.lock().await;

        let alive = match guard.as_mut() {
            Some(tunnel) => tunnel.is_running(),
            None => false,
        };

        if !alive {
            // **Stopped, not dropped.** A provider that runs a process
            // per service reports "not running" the moment one of them
            // has gone, and the others are still up — still publishing an
            // environment, and no longer held by anything that could take
            // them down. Dropping the box would strand them for the life
            // of the daemon.
            if let Some(tunnel) = guard.take() {
                tunnel.stop().await;
            }
            self.set_hostnames(None);
        }

        alive
    }

    /// What arrives from outside, if anything does.
    pub fn hostnames(&self) -> Option<Hostnames> {
        self.active
            .read()
            .ok()
            .and_then(|hostnames| hostnames.as_ref().cloned())
    }

    /// The zone being served, for the places that name one.
    ///
    /// `None` both when nothing is up and when what is up has no zone to
    /// name. The two are the same to a reader looking for a domain.
    pub fn domain(&self) -> Option<String> {
        self.hostnames()
            .as_ref()
            .and_then(Hostnames::domain)
            .map(str::to_string)
    }

    fn set_hostnames(&self, hostnames: Option<Hostnames>) {
        if let Ok(mut guard) = self.active.write() {
            *guard = hostnames;
        }
    }

    /// Starts the tunnel, replacing one that is already up.
    ///
    /// Replacing rather than refusing means `tunnel enable --domain` with
    /// a new domain does the obvious thing.
    pub async fn start(
        &self,
        configured: &Configured,
    ) -> Result<StartOutcome, kobune_tunnel::TunnelError> {
        let mut guard = self.running.lock().await;

        if let Some(existing) = guard.take() {
            existing.stop().await;
        }

        // **Cleared as the old tunnel goes, not when the new one
        // arrives.** What is between the two is nothing running, and a
        // `start` that fails — a rate limit, a service that never
        // announced a name — leaves exactly that. Names left here would
        // go on being routed to and advertised in every status response,
        // for hostnames that stopped existing when the tunnel above did.
        self.set_hostnames(None);

        let started = configured.provider.start(&configured.request).await?;
        // Asked of the tunnel rather than taken from the request: a name
        // handed out at connect time is not in the request, and is the
        // only thing that will ever reach this machine.
        self.set_hostnames(Some(started.tunnel.hostnames().clone()));
        *guard = Some(started.tunnel);

        Ok(started.outcome)
    }

    /// Stops the tunnel. Doing nothing when it is already down.
    pub async fn stop(&self) {
        let mut guard = self.running.lock().await;
        if let Some(tunnel) = guard.take() {
            tunnel.stop().await;
        }
        self.set_hostnames(None);
    }
}

/// Builds the request for a record.
///
/// `local_port` is the proxy's plain-HTTP port. Without one the tunnel has
/// nowhere to send traffic, so enabling is refused rather than started
/// into a dead end — which is why the caller resolves the port first.
pub fn request_for(
    record: &TunnelRecord,
    dir: std::path::PathBuf,
    local_port: u16,
) -> TunnelRequest {
    TunnelRequest::new(dir, local_port)
        .with_name(&record.name)
        .with_domain(record.domain.clone())
        .settled(record.zone_routed)
}

/// Reports where setup stands, without running anything.
pub async fn info(
    record: Option<&TunnelRecord>,
    handle: &TunnelHandle,
    configured: Option<&Configured>,
) -> TunnelInfo {
    info_with_notes(record, handle, configured, Vec::new()).await
}

/// [`info`], plus what the call that produced it changed.
pub async fn info_with_notes(
    record: Option<&TunnelRecord>,
    handle: &TunnelHandle,
    configured: Option<&Configured>,
    notes: Vec<String>,
) -> TunnelInfo {
    let Some(record) = record else {
        return TunnelInfo::disabled();
    };

    // Only asked about a tunnel somebody wants up. Readiness is a `PATH`
    // lookup and a stat, and the answer for a disabled one reads neither
    // — `status`, `disable` and every `doctor` run would pay for it.
    let readiness = record
        .enabled
        .then(|| configured.map(Configured::readiness))
        .flatten();

    // Turned off outranks everything. Reporting "needs login" about a
    // feature somebody deliberately switched off is noise, and `doctor`
    // would carry it on every run.
    let state = if !record.enabled {
        TunnelState::Disabled
    } else {
        match readiness {
            Some(Readiness::NotInstalled) => TunnelState::NotInstalled,
            Some(Readiness::NeedsLogin) => TunnelState::NeedsLogin,
            _ if handle.is_running().await => TunnelState::Running,
            _ => TunnelState::Stopped,
        }
    };

    // Only for a state that is actually waiting on the user — a tunnel
    // somebody turned off does not need to be told what to install. The
    // words are the provider's, since "install cloudflared" is the right
    // sentence for exactly one of them.
    let setup = match state {
        TunnelState::NotInstalled | TunnelState::NeedsLogin => configured
            .zip(readiness.as_ref())
            .and_then(|(configured, readiness)| configured.provider.missing(readiness))
            .map(|missing| missing.commands)
            .unwrap_or_default(),
        _ => Vec::new(),
    };

    TunnelInfo {
        state,
        provider: record.provider.clone(),
        domain: record.domain.clone(),
        record: configured
            .and_then(|configured| configured.provider.dns_record(&configured.request)),
        setup,
        notes,
        // Anything enabled was acknowledged with `--public`, unless the
        // provider guards it itself — in which case there was nothing to
        // acknowledge.
        public: record.enabled,
        // The enum, not the sentence. Wording it belongs to whoever is
        // drawing the screen.
        //
        // **Asked of the provider the record names, not of
        // `configured`.** What guards a tunnel is a constant per provider
        // and has nothing to do with where the proxy is listening —
        // which is the other reason `configured` is absent. Falling back
        // to "Kobune cannot see" there would send a quick tunnel's user
        // looking for an access policy on a hostname that is not theirs.
        access: kobune_tunnel::create(&record.provider)
            .map(|provider| access_of(provider.access()))
            .unwrap_or_default(),
    }
}

/// The provider's word for it, in the API's vocabulary.
fn access_of(access: Access) -> TunnelAccess {
    match access {
        Access::Managed => TunnelAccess::Managed,
        Access::Unknown { .. } => TunnelAccess::Unknown,
        Access::Open => TunnelAccess::Open,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(enabled: bool) -> TunnelRecord {
        TunnelRecord {
            provider: kobune_core::DEFAULT_TUNNEL_PROVIDER.into(),
            name: "kobune".into(),
            domain: Some("example.com".into()),
            enabled,
            zone_routed: true,
        }
    }

    /// A tunnel that says whether it was stopped or merely dropped.
    struct Recorded {
        alive: bool,
        hostnames: Hostnames,
        stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait::async_trait]
    impl RunningTunnel for Recorded {
        fn hostnames(&self) -> &Hostnames {
            &self.hostnames
        }

        fn is_running(&mut self) -> bool {
            self.alive
        }

        async fn stop(self: Box<Self>) {
            self.stopped
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn a_tunnel_found_dead_is_stopped_rather_than_dropped() {
        // **A provider that runs a process per service is not all or
        // nothing.** It reports "not running" as soon as one of them has
        // gone, and the rest are still up, still publishing — and once
        // the handle has let go of them, nothing can take them down for
        // the life of the daemon.
        let handle = TunnelHandle::default();
        let stopped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        *handle.running.lock().await = Some(Box::new(Recorded {
            alive: false,
            hostnames: Hostnames::Assigned(Default::default()),
            stopped: stopped.clone(),
        }));

        assert!(!handle.is_running().await);
        assert!(
            stopped.load(std::sync::atomic::Ordering::SeqCst),
            "what it was still holding was taken down"
        );
        assert!(handle.hostnames().is_none(), "and stops being advertised");
    }

    /// A provider whose `start` never gets anywhere.
    struct Refuses;

    #[async_trait::async_trait]
    impl TunnelProvider for Refuses {
        fn id(&self) -> &'static str {
            "refuses"
        }
        fn display_name(&self) -> &'static str {
            "a tunnel that will not start"
        }
        fn needs(&self) -> kobune_tunnel::Needs {
            kobune_tunnel::Needs {
                domain: false,
                targets: true,
            }
        }
        fn access(&self) -> Access {
            Access::Open
        }
        fn readiness(&self, _request: &TunnelRequest) -> Readiness {
            Readiness::Ready
        }
        fn missing(&self, _readiness: &Readiness) -> Option<kobune_tunnel::Missing> {
            None
        }
        fn dns_record(&self, _request: &TunnelRequest) -> Option<String> {
            None
        }
        async fn start(
            &self,
            _request: &TunnelRequest,
        ) -> Result<kobune_tunnel::Started, kobune_tunnel::TunnelError> {
            Err(kobune_tunnel::TunnelError::failed(
                "starting the tunnel",
                "no hostname was ever handed out",
            ))
        }
        fn leftovers(&self, _request: &TunnelRequest) -> kobune_tunnel::Leftover {
            kobune_tunnel::Leftover::default()
        }
    }

    #[tokio::test]
    async fn a_start_that_fails_stops_the_old_names_being_advertised() {
        // **`start` replaces, which means it takes the old tunnel down
        // first.** A failure after that point is nothing running at all,
        // and hostnames left behind would go on being routed to and
        // printed as URLs — for names that died with the tunnel that was
        // stopped to make room.
        let handle = TunnelHandle::default();
        let stopped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        *handle.running.lock().await = Some(Box::new(Recorded {
            alive: true,
            hostnames: Hostnames::Assigned(Default::default()),
            stopped: stopped.clone(),
        }));
        handle.set_hostnames(Some(Hostnames::Assigned(std::collections::BTreeMap::from(
            [(
                kobune_tunnel::TunnelTarget::new("myapp", None, "web"),
                "restless-mode-1234.trycloudflare.com".to_string(),
            )],
        ))));

        let configured = Configured {
            provider: Box::new(Refuses),
            request: TunnelRequest::new("/tmp", 80),
        };

        assert!(handle.start(&configured).await.is_err());
        assert!(
            stopped.load(std::sync::atomic::Ordering::SeqCst),
            "the tunnel that was up was stopped to make room"
        );
        assert!(
            handle.hostnames().is_none(),
            "and what it published stopped being advertised with it"
        );
    }

    #[tokio::test]
    async fn what_guards_a_tunnel_does_not_depend_on_the_proxy() {
        // `Configured` is absent when the proxy has no plain-HTTP port —
        // an ordinary state, and one `doctor` reports. What is in front
        // of a tunnel is the provider's answer either way, and reporting
        // "Kobune cannot see" would send a quick tunnel's user looking
        // for an access policy on a hostname that is not theirs.
        let handle = TunnelHandle::default();
        let mut record = record(true);
        record.provider = "quick".into();

        let info = info(Some(&record), &handle, None).await;

        assert_eq!(info.access, TunnelAccess::Open);
    }

    /// A provider pointed at a program, so readiness is the test's to set.
    fn configured(program: &str) -> Configured {
        Configured {
            provider: Box::new(kobune_tunnel::CloudflareProvider::with_program(program)),
            request: TunnelRequest::new("/tmp", 80).with_domain(Some("example.com".into())),
        }
    }

    #[tokio::test]
    async fn no_record_reads_as_disabled() {
        let handle = TunnelHandle::default();
        let info = info(None, &handle, None).await;

        assert_eq!(info.state, TunnelState::Disabled);
        assert!(info.domain.is_none());
    }

    #[tokio::test]
    async fn a_missing_program_outranks_being_enabled() {
        // Enabled in the state file but nothing to run it with. Reporting
        // "stopped" would send someone looking for the wrong problem.
        let handle = TunnelHandle::default();
        let configured = configured("kobune-definitely-not-a-real-program");

        let info = info(Some(&record(true)), &handle, Some(&configured)).await;

        assert_eq!(info.state, TunnelState::NotInstalled);
        assert!(!info.setup.is_empty(), "it says what to install");
    }

    #[tokio::test]
    async fn a_configured_but_stopped_tunnel_keeps_its_domain() {
        // `disable` clears the flag and keeps the record, so re-enabling
        // does not mean naming the domain again.
        let handle = TunnelHandle::default();
        let configured = configured("/bin/sh");

        let info = info(Some(&record(false)), &handle, Some(&configured)).await;

        assert_eq!(info.state, TunnelState::Disabled);
        assert_eq!(info.domain.as_deref(), Some("example.com"));
    }

    #[tokio::test]
    async fn a_tunnel_that_is_off_is_not_told_what_to_install() {
        // Whatever is missing, nobody asked for it to be there.
        let handle = TunnelHandle::default();
        let configured = configured("kobune-definitely-not-a-real-program");

        let info = info(Some(&record(false)), &handle, Some(&configured)).await;

        assert_eq!(info.state, TunnelState::Disabled);
        assert!(info.setup.is_empty(), "got: {:?}", info.setup);
    }

    #[tokio::test]
    async fn status_names_the_record_the_zone_needs() {
        // "Nothing resolves" usually means the DNS route is missing, and
        // the answer is the record to look for.
        let handle = TunnelHandle::default();
        let configured = configured("/bin/sh");

        let info = info(Some(&record(true)), &handle, Some(&configured)).await;

        assert_eq!(info.record.as_deref(), Some("*.example.com"));
    }

    #[tokio::test]
    async fn the_provider_travels_with_the_answer() {
        // Every field beside it is about one service's way of working, so
        // a reader needs to know which service it is reading about.
        let handle = TunnelHandle::default();
        let configured = configured("/bin/sh");

        let info = info(Some(&record(true)), &handle, Some(&configured)).await;

        assert_eq!(info.provider, kobune_core::DEFAULT_TUNNEL_PROVIDER);
    }

    #[tokio::test]
    async fn nothing_running_reads_as_not_running() {
        let handle = TunnelHandle::default();

        assert!(!handle.is_running().await);
        assert!(handle.domain().is_none(), "no domain to advertise");
    }
}
