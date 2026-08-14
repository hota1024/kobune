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
use minato_tunnel::StepOutcome;

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
        //
        // Normalised on the way in, because it is compared against the
        // stored one below and goes into every hostname: `Example.com.`
        // and `example.com` are the same zone, and left as typed they
        // would read as a change of domain and advertise a URL whose case
        // does not match the routing table's key.
        let domain = domain
            .or_else(|| existing.as_ref().map(|record| record.domain.clone()))
            .map(|domain| domain.trim().trim_end_matches('.').to_ascii_lowercase())
            .filter(|domain| !domain.is_empty())
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

        // Only asked when there is something to say. Once a zone is known
        // to be routed, the answer changes nothing and a lookup on every
        // enable is a round trip for nobody.
        let resolves = if record.zone_routed {
            true
        } else {
            minato_tunnel::process::wildcard_resolves(&settings).await
        };

        let notes = zone_notes(&record, dns, resolves);

        let mut record = record;
        // **Only when the route took effect**, which means both that
        // cloudflared accepted it and that the name answers. Either half
        // alone silences the warning for a zone that is still broken:
        // `AlreadyThere` would claim a record Minato did not put there,
        // and `Done` on its own is what a domain outside the login's zone
        // returns while resolving nowhere. The warning has to outlast one
        // run, because so does the problem.
        record.zone_routed = record.zone_routed || (dns == StepOutcome::Done && resolves);
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

        // The DNS records are named as well as the tunnel. Left behind
        // pointing at a tunnel that no longer exists, a record answers
        // with Cloudflare's error 1033 — worse than the NXDOMAIN it
        // replaced, and it does not expire.
        //
        // `*.{domain}` is this build's. The per-project records are from
        // before the hostname was flattened; Minato created them, never
        // deletes them, and stopped writing them, so the only place they
        // are still named is here.
        let mut notes = vec![format!(
            "the DNS record *.{} has no command — `cloudflared tunnel route \
             dns` only creates. Remove it in the Cloudflare dashboard",
            record.domain
        )];

        let older: Vec<String> = self
            .known_projects()
            .await
            .unwrap_or_default()
            .iter()
            .map(|project| format!("*.{project}.{}", record.domain))
            .collect();

        if !older.is_empty() {
            notes.push(format!(
                "and, from before tunnel hostnames were one label, {}",
                older.join(", ")
            ));
        }

        Some(minato_api::TunnelLeftover {
            domain: Some(record.domain.clone()),
            commands: vec![format!(
                "cloudflared tunnel delete --force {}",
                minato_tunnel::DEFAULT_TUNNEL_NAME
            )],
            notes,
        })
    }
}

/// What `enable` should say about the zone, given what routing did.
///
/// Only about the transition. Once Minato has routed a zone, repeating
/// any of this on every run turns it into noise, and a warning that is
/// always there is a warning nobody reads.
///
/// **One short line per element**, not a paragraph. A line here sets the
/// panel's preferred width, and `panel::wrap` breaks at the column rather
/// than at a space — deliberately, since what usually overflows a panel is
/// a path or a command. Prose has to arrive pre-broken.
fn zone_notes(record: &TunnelRecord, dns: StepOutcome, resolves: bool) -> Vec<String> {
    // Nothing to report about a zone Minato has already routed.
    if record.zone_routed {
        return Vec::new();
    }

    let wildcard = format!("*.{}", record.domain);

    // **The name does not answer, whatever cloudflared said.** This is
    // what a domain that is not the login's zone looks like from here:
    // `route dns` takes the hostname as relative to the zone the
    // certificate covers, creates `*.{domain}.{that zone}`, and exits 0.
    // Nothing else in the response can tell — `running` is true, the
    // tunnel is up, and no URL under `domain` will ever arrive.
    if !resolves {
        return vec![
            format!("{wildcard} does not resolve, so nothing arrives."),
            "The likely cause is that it is not the zone your".to_string(),
            "`cloudflared tunnel login` covers: the record is then".to_string(),
            format!("created as {wildcard}.<that zone> and this reports"),
            "success. Check the zone in the Cloudflare dashboard.".to_string(),
        ];
    }

    let mut notes = match dns {
        // The record reaches this tunnel and the name answers. Worth
        // saying once what that covers: it answers for every name in the
        // zone with none of its own, so a name that used to be NXDOMAIN
        // now reaches this machine — including the ones an ACME HTTP-01
        // challenge uses.
        StepOutcome::Done => vec![
            format!("{wildcard} now points here."),
            "Names with a record of their own are unaffected;".to_string(),
            "any other name in the zone reaches this machine.".to_string(),
        ],

        // Someone else's record, or one from an earlier install Minato has
        // no memory of. It resolves, but cloudflared only says the name is
        // taken, not what it points at, and if it is not this tunnel then
        // nothing arrives and everything above still reports `running`.
        StepOutcome::AlreadyThere => vec![
            format!("a DNS record for {wildcard} was already there,"),
            "and Minato did not create it. If it does not point".to_string(),
            "at this tunnel, no hostname will arrive.".to_string(),
        ],
    };

    // A resolving wildcard says the zone is right, but not that the
    // certificate reaches. Universal SSL covers one level below the zone,
    // so a domain that is itself a subdomain puts every hostname out of
    // range — a TLS handshake failure with everything here still saying
    // `running`. Minato cannot tell a zone from a subdomain of one without
    // the public suffix list, so this asks rather than refuses: getting
    // `example.co.uk` wrong would be worse than the question.
    if record.domain.split('.').count() > 2 {
        notes.push(format!(
            "if {} is not the zone itself, https will fail:",
            record.domain
        ));
        notes.push("its certificate covers one level below the zone.".to_string());
    }

    notes
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
        record_for("example.com", zone_routed)
    }

    fn record_for(domain: &str, zone_routed: bool) -> TunnelRecord {
        TunnelRecord {
            name: "minato".into(),
            domain: domain.into(),
            enabled: true,
            zone_routed,
        }
    }

    /// The wildcard answers — the case where the note is about scope.
    const RESOLVES: bool = true;

    fn joined(notes: &[String]) -> String {
        notes.join(" ")
    }

    #[test]
    fn a_record_someone_else_owns_is_called_out() {
        // The failure this prevents is total and silent: the tunnel runs,
        // `status` says running, and every hostname resolves to whatever
        // the pre-existing record points at instead.
        let notes = zone_notes(&record(false), StepOutcome::AlreadyThere, RESOLVES);
        let text = joined(&notes);

        assert!(text.contains("*.example.com"), "got: {notes:?}");
        assert!(text.contains("did not create it"), "got: {notes:?}");
    }

    #[test]
    fn a_wildcard_that_does_not_resolve_outranks_cloudflared_s_success() {
        // Seen in the wild: `--domain` naming a zone the cloudflared login
        // does not cover. `route dns` takes the hostname as relative to
        // the zone the certificate is scoped to, creates
        // `*.other.example.com`, and exits 0 — so `Done` here means
        // nothing, and every URL under `other` is unreachable while the
        // tunnel reports `running`.
        let notes = zone_notes(&record_for("other", false), StepOutcome::Done, false);
        let text = joined(&notes);

        assert!(text.contains("does not resolve"), "got: {notes:?}");
        assert!(
            text.contains("login"),
            "it names the likely cause: {notes:?}"
        );
        assert!(
            !text.contains("now points here"),
            "it must not also claim success: {notes:?}"
        );
    }

    #[test]
    fn notes_are_short_enough_to_sit_in_a_panel() {
        // A note sets the panel's preferred width, and the wrap breaks at
        // the column rather than at a space, so a long line is rendered
        // hyphen-free mid-word.
        for outcome in [StepOutcome::Done, StepOutcome::AlreadyThere] {
            for resolves in [true, false] {
                for note in zone_notes(&record(false), outcome, resolves) {
                    assert!(note.len() <= 64, "{} chars: {note}", note.len());
                }
            }
        }
    }

    #[test]
    fn a_domain_below_the_zone_is_questioned() {
        // Getting this wrong reproduces the handshake failure the flat
        // hostname exists to avoid, and nothing else would show it.
        let notes = zone_notes(
            &record_for("dev.example.com", false),
            StepOutcome::Done,
            RESOLVES,
        );

        assert!(
            joined(&notes).contains("is not the zone itself"),
            "got: {notes:?}"
        );
    }

    #[test]
    fn a_two_label_domain_is_not_questioned() {
        let notes = zone_notes(&record(false), StepOutcome::Done, RESOLVES);

        assert!(
            !joined(&notes).contains("is not the zone"),
            "got: {notes:?}"
        );
    }

    #[test]
    fn routing_the_record_says_what_it_now_covers() {
        let notes = zone_notes(&record(false), StepOutcome::Done, RESOLVES);

        assert!(joined(&notes).contains("*.example.com"), "got: {notes:?}");
    }

    #[test]
    fn a_zone_already_routed_by_minato_says_nothing() {
        // Both steps run on every enable and on every daemon start. A
        // warning that appears every time is a warning nobody reads.
        assert!(
            zone_notes(&record(true), StepOutcome::AlreadyThere, RESOLVES).is_empty(),
            "the usual case is silent"
        );
        assert!(
            zone_notes(&record(true), StepOutcome::Done, RESOLVES).is_empty(),
            "re-routed after being deleted by hand is still not news"
        );
    }
}
