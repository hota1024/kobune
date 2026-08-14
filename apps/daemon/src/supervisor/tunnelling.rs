//! Owning the Cloudflare Tunnel: enabling it, taking it down, and
//! bringing it back after a restart.
//!
//! **One named tunnel per machine**, carrying every project, with the
//! project inside the hostname's single label (`docs/DESIGN.md` §9).
//! Everything after `cloudflared tunnel login` is non-interactive and the
//! daemon does it — `tunnel create` and `tunnel route dns` run on every
//! enable and every start, with "it already exists" read as success,
//! because skipping them on a stored flag would trust that flag over
//! Cloudflare. What the state file does hold is whether Minato has routed
//! this zone before — not to skip the call, but to tell its own record
//! apart from one that was already there.

use minato_api::{ApiError, ErrorCode, Response, Target};
use minato_core::TunnelRecord;
use minato_runtime::EventSink;
use minato_tunnel::DnsOutcome;

use crate::tunnel;

use super::Supervisor;

impl Supervisor {
    /// Sets up the Cloudflare Tunnel and starts it.
    ///
    /// Idempotent: creating the tunnel and routing DNS both treat "it
    /// already exists" as success, so this is the same call whether the
    /// machine has been set up before or not.
    pub(super) async fn tunnel_enable(
        &self,
        target: Target,
        domain: Option<String>,
        public: bool,
        events: &EventSink,
    ) -> Result<Response, ApiError> {
        let context = self.resolve_project_only(&target).await?;
        let existing = self.tunnel_record().await?;

        // A domain given once is remembered, so re-enabling does not mean
        // naming it again.
        let domain = domain
            .or_else(|| existing.as_ref().map(|record| record.domain.clone()))
            .ok_or_else(|| {
                ApiError::new(
                    ErrorCode::InvalidConfig,
                    "no domain for the tunnel".to_string(),
                )
                .with_hint("name the Cloudflare zone with --domain example.com")
            })?;

        // Minato cannot apply a Cloudflare Access policy: that needs the
        // API, and everything here goes through the CLI so there is no
        // token to obtain or store. Since it cannot promise the policy is
        // there, it will not put an environment on the public internet
        // without being asked (`docs/DESIGN.md` §9).
        if !public {
            return Err(ApiError::new(
                ErrorCode::Unsupported,
                "a tunnel exposes this environment to the internet".to_string(),
            )
            .with_hint(
                "put a Cloudflare Access policy in front of the hostname, then \
                 re-run with --public to confirm. Minato cannot apply the policy \
                 itself — that needs the Cloudflare API, not cloudflared",
            ));
        }

        // Whether this zone's record is one Minato has put in place before.
        // A domain that has just changed starts over: the old zone's record
        // says nothing about the new one's.
        let zone_routed = existing
            .as_ref()
            .is_some_and(|record| record.zone_routed && record.domain == domain);

        let record = TunnelRecord {
            name: existing
                .as_ref()
                .map(|record| record.name.clone())
                .unwrap_or_else(|| minato_tunnel::DEFAULT_TUNNEL_NAME.to_string()),
            domain,
            enabled: true,
            zone_routed,
        };

        let settings = self.tunnel_settings(&record)?;

        // Nothing to run before cloudflared is installed and logged in,
        // and login opens a browser. Report the step instead of failing:
        // the state is legitimate and the answer is a command to run.
        let readiness = minato_tunnel::readiness(&settings);
        if !readiness.is_ready() {
            return Ok(Response::Tunnel(
                tunnel::info(Some(&record), &self.tunnel, Some(&settings)).await,
            ));
        }

        events.step_started("tunnel", "starting the tunnel");
        let dns = match self.tunnel.start(settings.clone()).await {
            Ok(dns) => {
                events.step_done("tunnel", "starting the tunnel");
                dns
            }
            Err(err) => {
                events.step_failed("tunnel", "starting the tunnel", err.to_string());
                return Err(tunnel_error(err));
            }
        };

        let notes = zone_notes(&record, dns);

        let mut record = record;
        record.zone_routed = true;
        self.save_tunnel_record(Some(record.clone())).await?;

        // The routing table is rebuilt so the tunnel hostnames resolve.
        // Without this the tunnel is up and every request through it 404s
        // until something else happens to refresh.
        self.refresh(&context.project, &context.config).await?;

        Ok(Response::Tunnel(
            tunnel::info_with_notes(Some(&record), &self.tunnel, Some(&settings), notes).await,
        ))
    }
    /// Stops the tunnel, keeping the record.
    ///
    /// The named tunnel and its DNS records stay in Cloudflare: they cost
    /// nothing idle, and deleting them would put `cloudflared tunnel
    /// login` back in the path of re-enabling.
    pub(super) async fn tunnel_disable(&self, target: Target) -> Result<Response, ApiError> {
        let context = self.resolve_project_only(&target).await?;

        self.tunnel.stop().await;

        let record = match self.tunnel_record().await? {
            Some(mut record) => {
                record.enabled = false;
                self.save_tunnel_record(Some(record.clone())).await?;
                Some(record)
            }
            None => None,
        };

        // Drops the tunnel hostnames from the routing table.
        self.refresh(&context.project, &context.config).await?;

        let settings = record
            .as_ref()
            .and_then(|record| self.tunnel_settings(record).ok());

        Ok(Response::Tunnel(
            tunnel::info(record.as_ref(), &self.tunnel, settings.as_ref()).await,
        ))
    }
    /// Reports where the tunnel stands. Runs nothing.
    pub(super) async fn tunnel_status(&self, target: Target) -> Result<Response, ApiError> {
        // Nothing in the answer is per-project any more, but the target is
        // still resolved: `tunnel status` run somewhere that is not a
        // Minato project should say so rather than report on a tunnel the
        // caller has nothing to do with.
        self.resolve_project_only(&target).await?;
        let record = self.tunnel_record().await?;

        let settings = record
            .as_ref()
            .and_then(|record| self.tunnel_settings(record).ok());

        Ok(Response::Tunnel(
            tunnel::info(record.as_ref(), &self.tunnel, settings.as_ref()).await,
        ))
    }
    /// The tunnel as the state store has it.
    pub async fn tunnel_record(&self) -> Result<Option<TunnelRecord>, ApiError> {
        let _guard = self.state_lock.lock().await;
        let state = self.store.load().map_err(ApiError::from)?;
        Ok(state.tunnel)
    }
    async fn save_tunnel_record(&self, record: Option<TunnelRecord>) -> Result<(), ApiError> {
        let _guard = self.state_lock.lock().await;

        self.store
            .update(|state| {
                state.tunnel = record;
                Ok(())
            })
            .map_err(ApiError::from)
    }
    /// Builds the settings for a record.
    ///
    /// Fails when the proxy has no plain-HTTP port: the tunnel would have
    /// nowhere to send traffic, and starting it would publish hostnames
    /// that only ever 502.
    pub fn tunnel_settings(
        &self,
        record: &TunnelRecord,
    ) -> Result<minato_tunnel::TunnelSettings, ApiError> {
        let port = self.gateway.http_port().ok_or_else(|| {
            ApiError::new(
                ErrorCode::RuntimeUnavailable,
                "the HTTP proxy is not listening, so the tunnel has nowhere to \
                 forward to"
                    .to_string(),
            )
            .with_hint("check `minato doctor`")
        })?;

        Ok(tunnel::settings_for(record, self.paths.tunnel_dir(), port))
    }
    /// Brings the tunnel up at daemon start, when the state says it was on.
    ///
    /// Failing here does not stop the daemon. The local URLs work either
    /// way, and taking everything down because Cloudflare is unreachable
    /// would be the wrong trade.
    pub async fn restore_tunnel(&self) {
        let record = match self.tunnel_record().await {
            Ok(Some(record)) if record.enabled => record,
            Ok(_) => return,
            Err(err) => {
                tracing::warn!("cannot read the tunnel state: {err}");
                return;
            }
        };

        let settings = match self.tunnel_settings(&record) {
            Ok(settings) => settings,
            Err(err) => {
                tracing::warn!("not starting the tunnel: {err}");
                return;
            }
        };

        if !minato_tunnel::readiness(&settings).is_ready() {
            tracing::warn!(
                "the tunnel is enabled but cloudflared is not ready. \
                 Run `minato tunnel status` for the remaining steps"
            );
            return;
        }

        match self.tunnel.start(settings).await {
            Ok(_) => tracing::info!("tunnel restored for *.{}", record.domain),
            Err(err) => tracing::warn!("cannot start the tunnel: {err}"),
        }
    }
    /// Stops the tunnel and says what is left in the Cloudflare account.
    ///
    /// The local half — the `cloudflared` process and the record in the
    /// state file — is Minato's to clean up and it does. The named tunnel
    /// and its DNS records are in the user's account, and an uninstaller
    /// that reached in there uninvited would be doing something no other
    /// command in this project does. So they are reported instead, with
    /// the command that removes them.
    pub(super) async fn purge_tunnel(
        &self,
        dry_run: bool,
        events: &EventSink,
    ) -> Option<minato_api::TunnelLeftover> {
        let record = {
            let _guard = self.state_lock.lock().await;
            self.store.load().ok()?.tunnel.clone()?
        };

        if !dry_run {
            events.step_started("tunnel", "stopping the tunnel");
            self.tunnel.stop().await;
            events.step_done("tunnel", "stopping the tunnel");

            let _guard = self.state_lock.lock().await;
            let _ = self.store.update(|state| {
                state.tunnel = None;
                Ok(())
            });
        }

        // The DNS record is named as well as the tunnel. It covers the
        // whole zone, and left behind pointing at a tunnel that no longer
        // exists it answers every unrecorded name in the zone with
        // Cloudflare's error 1033 — worse than the NXDOMAIN it replaced,
        // and it does not expire.
        Some(minato_api::TunnelLeftover {
            domain: Some(record.domain.clone()),
            commands: vec![
                format!(
                    "cloudflared tunnel delete --force {}",
                    minato_tunnel::DEFAULT_TUNNEL_NAME
                ),
                format!(
                    "# and remove the DNS record *.{} in the Cloudflare dashboard",
                    record.domain
                ),
            ],
        })
    }
}

/// What `enable` should say about the zone, given what routing did.
///
/// Only about the transition. Once Minato has routed a zone, repeating
/// either of these on every run turns them into noise, and a warning that
/// is always there is a warning nobody reads.
fn zone_notes(record: &TunnelRecord, dns: DnsOutcome) -> Vec<String> {
    let wildcard = format!("*.{}", record.domain);

    match (record.zone_routed, dns) {
        // Minato routed it before and Cloudflare agrees it is there.
        (true, _) => Vec::new(),

        // The record reaches this tunnel. Worth saying once what that now
        // covers: it answers for every name in the zone that has none of
        // its own, so a name that used to be NXDOMAIN now reaches this
        // machine — including the ones an ACME HTTP-01 challenge uses.
        (false, DnsOutcome::Routed) => vec![format!(
            "{wildcard} now points here. Names in the zone with a record of \
             their own are unaffected; any other name reaches this machine \
             and gets a 404"
        )],

        // Someone else's record, or one from an earlier install Minato has
        // no memory of. cloudflared only says the name is taken, not what
        // it points at, and if it is not this tunnel then nothing arrives
        // and everything above still reports `running`.
        (false, DnsOutcome::AlreadyExisted) => vec![format!(
            "a DNS record for {wildcard} was already there, and Minato did \
             not create it. If it does not point at this tunnel, no hostname \
             will arrive — check it in the Cloudflare dashboard"
        )],
    }
}

/// Maps a tunnel failure onto the API's vocabulary.
fn tunnel_error(err: minato_tunnel::TunnelError) -> ApiError {
    use minato_tunnel::TunnelError;

    let message = err.to_string();
    match err {
        TunnelError::NotInstalled(_) => ApiError::new(ErrorCode::Unsupported, message)
            .with_hint("install cloudflared (brew install cloudflared)"),
        TunnelError::NotLoggedIn => ApiError::new(ErrorCode::RuntimeUnavailable, message)
            .with_hint("run `cloudflared tunnel login`"),
        TunnelError::Write { .. } => ApiError::internal(message),
        TunnelError::Failed { .. } => ApiError::new(ErrorCode::RuntimeFailed, message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(zone_routed: bool) -> TunnelRecord {
        TunnelRecord {
            name: "minato".into(),
            domain: "example.com".into(),
            enabled: true,
            zone_routed,
        }
    }

    #[test]
    fn a_record_someone_else_owns_is_called_out() {
        // The failure this prevents is total and silent: the tunnel runs,
        // `status` says running, and every hostname resolves to whatever
        // the pre-existing record points at instead.
        let notes = zone_notes(&record(false), DnsOutcome::AlreadyExisted);

        assert_eq!(notes.len(), 1, "got: {notes:?}");
        assert!(notes[0].contains("*.example.com"), "got: {notes:?}");
        assert!(notes[0].contains("did not create it"), "got: {notes:?}");
    }

    #[test]
    fn routing_the_record_says_what_it_now_covers() {
        let notes = zone_notes(&record(false), DnsOutcome::Routed);

        assert_eq!(notes.len(), 1, "got: {notes:?}");
        assert!(notes[0].contains("*.example.com"), "got: {notes:?}");
    }

    #[test]
    fn a_zone_already_routed_by_minato_says_nothing() {
        // Both steps run on every enable and on every daemon start. A
        // warning that appears every time is a warning nobody reads.
        assert!(
            zone_notes(&record(true), DnsOutcome::AlreadyExisted).is_empty(),
            "the usual case is silent"
        );
        assert!(
            zone_notes(&record(true), DnsOutcome::Routed).is_empty(),
            "re-routed after being deleted by hand is still not news"
        );
    }
}
