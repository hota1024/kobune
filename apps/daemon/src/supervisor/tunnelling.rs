//! Owning the tunnel: enabling it, taking it down, and bringing it back
//! after a restart.
//!
//! **One named tunnel per machine**, carrying every project, with the
//! project inside the hostname's single label (`docs/DESIGN.md` §9).
//!
//! **What the tunnel service needed is not decided here.** Which steps are
//! interactive, what "already exists" means, what a zone is — all of that
//! is the provider's, reached through `kobune_tunnel::create`. What is
//! left here is the part that is the same whoever carries it: refusing to
//! publish without `--public`, remembering the record, and rebuilding the
//! routing table so the hostnames resolve.

use kobune_api::{ApiError, ErrorCode, Response, Target};
use kobune_core::TunnelRecord;
use kobune_runtime::EventSink;
use kobune_tunnel::Access;

use crate::tunnel::{self, Configured};

use super::Supervisor;

impl Supervisor {
    /// Sets the tunnel up and starts it.
    ///
    /// Idempotent, because every provider's setup is: "it already exists"
    /// reads as success, so this is the same call whether the machine has
    /// been set up before or not.
    pub(super) async fn tunnel_enable(
        &self,
        target: Target,
        provider: Option<String>,
        domain: Option<String>,
        public: bool,
        events: &EventSink,
    ) -> Result<Response, ApiError> {
        let context = self.resolve_project_only(&target).await?;
        let existing = self.tunnel_record().await?;

        // Named once and remembered, the same as the domain below.
        let provider = provider
            .or_else(|| existing.as_ref().map(|record| record.provider.clone()))
            .unwrap_or_else(|| kobune_core::DEFAULT_TUNNEL_PROVIDER.to_string());

        // A domain given once is remembered, so re-enabling does not mean
        // naming it again.
        //
        // Normalised on the way in, because it is compared against the
        // stored one below and goes into every hostname: `Example.com.`
        // and `example.com` are the same zone, and left as typed they
        // would read as a change of domain and advertise a URL whose case
        // does not match the routing table's key.
        //
        // **Not required here.** Whether one is needed at all is the
        // provider's to say, and it is asked below once it is built.
        let domain = domain
            .or_else(|| existing.as_ref().and_then(|record| record.domain.clone()))
            .map(|domain| domain.trim().trim_end_matches('.').to_ascii_lowercase())
            .filter(|domain| !domain.is_empty());

        // Whether this provider has confirmed this zone before. A domain
        // that has just changed starts over, and so does a change of
        // provider: what one of them saw work says nothing about another.
        let zone_routed = existing.as_ref().is_some_and(|record| {
            record.zone_routed && record.domain == domain && record.provider == provider
        });

        let mut record = TunnelRecord {
            provider,
            name: existing
                .as_ref()
                .map(|record| record.name.clone())
                .unwrap_or_else(|| kobune_tunnel::DEFAULT_TUNNEL_NAME.to_string()),
            domain,
            enabled: true,
            zone_routed,
        };

        let mut configured = self.tunnel_configured(&record)?;
        let needs = configured.provider.needs();

        // **A zone is remembered even by a provider with no use for one.**
        // The CLI's help says a domain named once need not be named again,
        // and a spell on a quick tunnel is not a reason to have to type it
        // back in. What it must not do is show up: `status` printing
        // `running  quick  *.example.com` over URLs that are all under
        // Cloudflare's own domain would say this tunnel is on that zone.
        // So it is kept out of the request and out of the answer — see
        // `tunnel::info_with_notes` — rather than out of the record.
        //
        // What the old provider left in an account is a different
        // question, and this is where it is asked: a switch is the moment
        // a tunnel and a DNS record stop being anything Kobune will
        // mention again.
        let mut left_behind = Vec::new();

        if !needs.domain {
            left_behind = self.what_the_last_provider_left(existing.as_ref(), &record.provider);
            configured.request = configured.request.with_domain(None);
        }

        // An incomplete command before a question about it. Both are
        // true of `kobune tunnel enable` with neither flag, and "you left
        // something out" is the one somebody can act on without deciding
        // anything.
        if needs.domain && record.domain.is_none() {
            return Err(ApiError::new(
                ErrorCode::InvalidConfig,
                format!(
                    "{} needs a zone of yours",
                    configured.provider.display_name()
                ),
            )
            .with_hint("name it with --domain example.com"));
        }

        // **Asked, not assumed.** Kobune will not put an environment on
        // the internet with nothing in front of it unless it was told to
        // (`docs/DESIGN.md` §9) — but what "nothing in front of it" means,
        // and what could be put there, is the provider's to say.
        if let Some(refusal) = refuse_without_public(configured.provider.access(), public) {
            return Err(refusal);
        }

        // **Only a provider that cannot work it out is told.** A wildcard
        // covers a workspace made a minute from now; one that hands out a
        // name per service can only reach what existed when it was asked,
        // so it is given this workspace's exposed services and says as
        // much in its own notes.
        if needs.targets {
            let resolved = self.resolve(&target).await?;
            configured.request = configured.request.with_targets(exposed_targets(&resolved));
        }

        // Nothing to run before the provider's own setup is done, and the
        // step that is left is usually one that opens a browser and waits.
        // Report it instead of failing: the state is legitimate and the
        // answer is a command to run.
        if !configured
            .provider
            .readiness(&configured.request)
            .is_ready()
        {
            // **What was up does not survive being replaced by
            // something that cannot run.** Every other way out of this
            // command replaces the running tunnel; this one used to
            // leave it up, publishing under a provider and a zone the
            // answer no longer mentions. `status` and `doctor` then read
            // `not installed` over an environment that is still on the
            // internet, which is the one reading nobody should be given.
            //
            // Not for the same tunnel asked for again: a binary that has
            // gone from `PATH` says nothing about the process still
            // carrying the traffic.
            if replaces_what_is_running(existing.as_ref(), &record) {
                self.tunnel.stop().await;
                self.refresh(&context.project, &context.config).await?;
            }

            // **Saved even though nothing started.** The CLI's help says
            // `--provider` is remembered, and the run that needs it
            // remembered is exactly this one: the first, on a machine
            // that is not set up yet. Without this, the next
            // `kobune tunnel enable --public` falls back to the default
            // provider and fails asking for a zone nobody wanted.
            self.save_tunnel_record(Some(record.clone())).await?;

            return Ok(Response::Tunnel(
                tunnel::info_with_notes(
                    Some(&record),
                    &self.tunnel,
                    Some(&configured),
                    left_behind,
                )
                .await,
            ));
        }

        events.step_started("tunnel", "starting the tunnel");
        let outcome = match self.tunnel.start(&configured).await {
            Ok(outcome) => {
                events.step_done("tunnel", "starting the tunnel");
                outcome
            }
            Err(err) => {
                events.step_failed("tunnel", "starting the tunnel", err.to_string());
                return Err(tunnel_error(&record.provider, err));
            }
        };

        // Whether the provider has now seen its own setup work. Only ever
        // towards `true`: what a note describes outlasts the run that
        // found it, so the silence has to be earned once rather than
        // assumed every time.
        record.zone_routed = outcome.settled;
        self.save_tunnel_record(Some(record.clone())).await?;

        // The routing table is rebuilt so the tunnel hostnames resolve.
        // Without this the tunnel is up and every request through it 404s
        // until something else happens to refresh.
        self.refresh(&context.project, &context.config).await?;

        // What the last provider left first: it is about a zone that has
        // just stopped being mentioned anywhere, where the rest is about
        // the tunnel that is now up.
        let mut notes = left_behind;
        notes.extend(outcome.notes);

        Ok(Response::Tunnel(
            tunnel::info_with_notes(Some(&record), &self.tunnel, Some(&configured), notes).await,
        ))
    }
    /// Stops the tunnel, keeping the record.
    ///
    /// Whatever the provider set up on its side stays: it costs nothing
    /// idle, and tearing it down would put the interactive setup step back
    /// in the path of re-enabling.
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

        let configured = record
            .as_ref()
            .and_then(|record| self.tunnel_configured(record).ok());

        Ok(Response::Tunnel(
            tunnel::info(record.as_ref(), &self.tunnel, configured.as_ref()).await,
        ))
    }
    /// Reports where the tunnel stands. Runs nothing.
    pub(super) async fn tunnel_status(&self, target: Target) -> Result<Response, ApiError> {
        // Nothing in the answer is per-project any more, but the target is
        // still resolved: `tunnel status` run somewhere that is not a
        // Kobune project should say so rather than report on a tunnel the
        // caller has nothing to do with.
        self.resolve_project_only(&target).await?;
        let record = self.tunnel_record().await?;

        let configured = record
            .as_ref()
            .and_then(|record| self.tunnel_configured(record).ok());

        Ok(Response::Tunnel(
            tunnel::info(record.as_ref(), &self.tunnel, configured.as_ref()).await,
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
    /// Builds the provider a record names, and what to ask it for.
    ///
    /// Fails when the proxy has no plain-HTTP port: the tunnel would have
    /// nowhere to send traffic, and starting it would publish hostnames
    /// that only ever 502. And when the record names a provider this build
    /// does not have — `TunnelRecord.provider` keeps an unrecognised value
    /// rather than failing to load, so it is here that it has to be
    /// refused instead of quietly becoming the default.
    pub fn tunnel_configured(&self, record: &TunnelRecord) -> Result<Configured, ApiError> {
        let port = self.gateway.http_port().ok_or_else(|| {
            ApiError::new(
                ErrorCode::RuntimeUnavailable,
                "the HTTP proxy is not listening, so the tunnel has nowhere to \
                 forward to"
                    .to_string(),
            )
            .with_hint("check `kobune doctor`")
        })?;

        let provider = kobune_tunnel::create(&record.provider)
            .map_err(|err| ApiError::new(ErrorCode::Unsupported, err.to_string()))?;

        Ok(Configured {
            provider,
            request: tunnel::request_for(record, self.paths.tunnel_dir(), port),
        })
    }
    /// The provider and record, for the questions that need no proxy.
    ///
    /// **What a tunnel left in somebody's account has nothing to do with
    /// where traffic goes.** Going through [`Self::tunnel_configured`]
    /// for it meant a proxy that failed to bind — an ordinary state, and
    /// one `doctor` reports — could silence an uninstall's report of the
    /// named tunnel and the DNS record it is leaving behind, in an
    /// account the user then has to find them in themselves.
    fn tunnel_described(&self, record: &TunnelRecord) -> Option<Configured> {
        let provider = kobune_tunnel::create(&record.provider).ok()?;

        Some(Configured {
            provider,
            // Zero, and unread: nothing asked of a provider here depends
            // on where the proxy is listening.
            request: tunnel::request_for(record, self.paths.tunnel_dir(), 0),
        })
    }
    /// What the provider a switch is leaving behind still has in an
    /// account of the user's.
    ///
    /// Nothing when the same provider carries on — it has not stopped
    /// being the one that put it there, and it is asked again at
    /// uninstall — and nothing when there was no zone to have set
    /// anything up on.
    fn what_the_last_provider_left(
        &self,
        previous: Option<&TunnelRecord>,
        now: &str,
    ) -> Vec<String> {
        let Some(previous) = previous else {
            return Vec::new();
        };

        let Some(domain) = previous.domain.clone().filter(|_| previous.provider != now) else {
            return Vec::new();
        };

        // Absent only for a provider this build does not have, which
        // leaves nothing truthful to say about what it set up.
        let Some(described) = self.tunnel_described(previous) else {
            return Vec::new();
        };

        left_in_the_account(described.provider.as_ref(), &described.request, &domain)
    }

    /// Brings the tunnel up at daemon start, when the state says it was on.
    ///
    /// Failing here does not stop the daemon. The local URLs work either
    /// way, and taking everything down because a tunnel service is
    /// unreachable would be the wrong trade.
    pub async fn restore_tunnel(&self) {
        let record = match self.tunnel_record().await {
            Ok(Some(record)) if record.enabled => record,
            Ok(_) => return,
            Err(err) => {
                tracing::warn!("cannot read the tunnel state: {err}");
                return;
            }
        };

        // **A tunnel that has to be told what to publish is not brought
        // back.** The names it handed out died with the processes that
        // held them, and starting again would hand out different ones —
        // reachable by nobody, since the links people have point at the
        // old names. What was published is not stored either, so there is
        // nothing to restore it from. `tunnel status` says `stopped`, and
        // enabling again is a deliberate act.
        //
        // **Asked before anything that needs the proxy.** What a provider
        // needs is a constant of the provider, and going through
        // `tunnel_configured` for it meant a proxy that had not bound —
        // it is started alongside this — could send the daemon home
        // before the record stopped claiming it should be running.
        if let Some(described) = self.tunnel_described(&record)
            && described.provider.needs().targets
        {
            tracing::info!(
                "not restoring the {} tunnel: the hostnames it handed out \
                 went with it. Run `kobune tunnel enable --public` for new ones",
                described.provider.display_name()
            );

            // **The record stops claiming it should be running.** Left
            // enabled with nothing up, `status` reads `stopped` and
            // `doctor` fails on it for as long as the machine stays up —
            // a red check about a state the design calls correct.
            let mut record = record;
            record.enabled = false;
            if let Err(err) = self.save_tunnel_record(Some(record)).await {
                tracing::warn!("cannot record that the tunnel did not come back: {err}");
            }

            return;
        }

        let mut configured = match self.tunnel_configured(&record) {
            Ok(configured) => configured,
            Err(err) => {
                tracing::warn!("not starting the tunnel: {err}");
                return;
            }
        };

        // **Nobody is waiting to be told what happened.** There is no
        // reply for a note to travel in and nothing here writes the
        // answer down, so a provider that would spend a round trip
        // working one out can skip it.
        configured.request = configured.request.explain(false);

        if !configured
            .provider
            .readiness(&configured.request)
            .is_ready()
        {
            tracing::warn!(
                "the tunnel is enabled but {} is not ready. \
                 Run `kobune tunnel status` for the remaining steps",
                configured.provider.display_name()
            );
            return;
        }

        match self.tunnel.start(&configured).await {
            Ok(_) => tracing::info!(
                "tunnel restored for {}",
                record.domain.as_deref().unwrap_or("this machine")
            ),
            Err(err) => tracing::warn!("cannot start the tunnel: {err}"),
        }
    }
    /// Stops the tunnel and says what is left in the user's own account.
    ///
    /// The local half — the running tunnel and the record in the state
    /// file — is Kobune's to clean up and it does. What the provider set
    /// up is in somebody's account, and an uninstaller that reached in
    /// there uninvited would be doing something no other command in this
    /// project does. So it is reported instead, in the provider's words,
    /// with the command that removes it.
    pub(super) async fn purge_tunnel(
        &self,
        dry_run: bool,
        events: &EventSink,
    ) -> Option<kobune_api::TunnelLeftover> {
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

        // What the provider left in the account, in its own words. It
        // knows what it created and what its own tooling can remove; a
        // record with no delete command can only be described.
        //
        // Absent only when the record names a provider this build does
        // not have. There is then nothing truthful to say about an
        // account Kobune cannot describe, and the local half has gone
        // regardless.
        let leftover = self
            .tunnel_described(&record)
            .map(|configured| configured.provider.leftovers(&configured.request))
            .unwrap_or_default();

        let mut notes = leftover.notes;

        // The per-project records are from before the hostname was
        // flattened; Kobune created them, never deletes them, and stopped
        // writing them, so the only place they are still named is here.
        // Kobune's own history rather than the provider's, which is why it
        // is added on this side.
        let older: Vec<String> = match record.domain.as_deref() {
            Some(domain) => self
                .known_projects()
                .await
                .unwrap_or_default()
                .iter()
                .map(|project| format!("*.{project}.{domain}"))
                .collect(),
            // No zone, so nothing was ever written to one.
            None => Vec::new(),
        };

        if let Some(note) = older_records_note(&older, &notes) {
            notes.push(note);
        }

        // **Nothing left means nothing said.** A provider that touched
        // no account of yours — a quick tunnel writes nothing anywhere —
        // has no leftovers, and the panel that renders this prints its
        // heading whenever there is a value at all. "left in your account:"
        // with nothing under it claims the opposite of the truth.
        if leftover.commands.is_empty() && notes.is_empty() {
            return None;
        }

        Some(kobune_api::TunnelLeftover {
            domain: record.domain.clone(),
            commands: leftover.commands,
            notes,
        })
    }
}

/// Whether what is running has stopped being what the record describes.
///
/// A different service, or the same one on a different zone: either way
/// the tunnel that is up publishes hostnames nothing will report any
/// more. The same tunnel asked for again is not one of these — the
/// answer would otherwise be to take down a working tunnel because
/// something has since gone from `PATH`.
fn replaces_what_is_running(previous: Option<&TunnelRecord>, now: &TunnelRecord) -> bool {
    previous
        .is_some_and(|previous| previous.provider != now.provider || previous.domain != now.domain)
}

/// What a provider left in an account, as lines somebody can act on.
///
/// **The provider's own commands**, one short line each: these are drawn
/// in a panel that wraps at the column rather than at a space, so prose
/// has to arrive pre-broken. Nothing at all for a provider that touched
/// no account of yours — there is then nothing to have been left.
fn left_in_the_account(
    provider: &dyn kobune_tunnel::TunnelProvider,
    request: &kobune_tunnel::TunnelRequest,
    domain: &str,
) -> Vec<String> {
    let leftover = provider.leftovers(request);
    let dns = provider.dns_record(request);

    if leftover.commands.is_empty() && dns.is_none() {
        return Vec::new();
    }

    // Not "it set this up": how far it got is not known here, and a
    // record saved before the provider was ever ready has nothing behind
    // it. What is true either way is that this is the last time Kobune
    // says the words.
    let mut notes = vec![format!(
        "kobune stops naming what {} has on {domain}",
        provider.display_name()
    )];

    notes.extend(
        leftover
            .commands
            .iter()
            .map(|command| format!("remove it with `{command}`")),
    );

    // Named separately because no command removes it — the record is
    // created by `tunnel route dns` and deleted in the dashboard.
    if let Some(dns) = dns {
        notes.push(format!("and the DNS record {dns}, in the dashboard"));
    }

    notes
}

/// The per-project records from before hostnames were one label.
///
/// **A continuation only when there is something to continue.** The
/// provider's own notes come first and this reads as the next of them;
/// with none, a line beginning "and," is the tail of something that was
/// never printed.
fn older_records_note(older: &[String], after: &[String]) -> Option<String> {
    if older.is_empty() {
        return None;
    }

    Some(format!(
        "{} from before tunnel hostnames were one label, {}",
        if after.is_empty() { "left" } else { "and," },
        older.join(", ")
    ))
}

/// Why publishing was refused, when `--public` was not said out loud.
///
/// **The refusal is the same and the advice is not.** Telling somebody to
/// put a Cloudflare Access policy in front of a `trycloudflare.com`
/// hostname is telling them to do something they cannot: the name is
/// Cloudflare's, and there is nothing of theirs to attach a policy to. A
/// hint that cannot be followed is worse than no hint.
fn refuse_without_public(access: Access, public: bool) -> Option<ApiError> {
    if public || !access.needs_acknowledging() {
        return None;
    }

    let error = ApiError::new(
        ErrorCode::Unsupported,
        "a tunnel exposes this environment to the internet".to_string(),
    );

    Some(match access {
        Access::Unknown { policy } => error.with_hint(format!(
            "put {policy} in front of the hostname, then re-run with \
             --public to confirm. Kobune cannot apply one itself, so it \
             cannot tell whether one is there"
        )),
        Access::Open => error.with_hint(
            "this tunnel has no access control at all — the hostname is the \
             service's, so there is nothing to put a policy on. Anyone with \
             the URL reaches this environment. Re-run with --public to accept \
             that",
        ),
        // Guarded against above; nothing to acknowledge.
        Access::Managed => error,
    })
}

/// The services of a workspace that may be published.
///
/// `expose = false` is left out here as it is everywhere else: a database
/// that cannot be reached from outside even by guessing is the point of
/// the flag, and a provider handed one would open a hostname straight to
/// it.
fn exposed_targets(resolved: &crate::resolve::Resolved) -> Vec<kobune_tunnel::TunnelTarget> {
    resolved
        .config
        .services
        .iter()
        .filter(|(_, service)| service.exposed())
        .map(|(name, _)| {
            kobune_tunnel::TunnelTarget::new(
                &resolved.project,
                resolved.workspace.url_label(),
                name,
            )
        })
        .collect()
}

/// Maps a tunnel failure onto the API's vocabulary.
///
/// **The code is neutral and the hint is not.** The same `NotInstalled`
/// arrives from any provider, so what it maps to is decided here — but
/// what to do about it names a program, and only the provider that was
/// asked knows which.
fn tunnel_error(provider: &str, err: kobune_tunnel::TunnelError) -> ApiError {
    use kobune_tunnel::TunnelError;

    let hint = kobune_tunnel::error_hint(provider, &err);
    let message = err.to_string();

    let error = match err {
        TunnelError::NotInstalled(_) | TunnelError::Unsupported(_) => {
            ApiError::new(ErrorCode::Unsupported, message)
        }
        TunnelError::NotLoggedIn => ApiError::new(ErrorCode::RuntimeUnavailable, message),
        TunnelError::Write { .. } => ApiError::internal(message),
        TunnelError::Failed { .. } => ApiError::new(ErrorCode::RuntimeFailed, message),
    };

    match hint {
        Some(hint) => error.with_hint(hint),
        None => error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use kobune_tunnel::TunnelError;

    fn a_policy_you_could_apply() -> Access {
        Access::Unknown {
            policy: "a Cloudflare Access policy".to_string(),
        }
    }

    #[test]
    fn publishing_is_refused_until_it_is_said_out_loud() {
        let refusal = refuse_without_public(a_policy_you_could_apply(), false).expect("refused");

        assert_eq!(refusal.code, ErrorCode::Unsupported);
        assert!(
            refusal
                .hint
                .as_deref()
                .is_some_and(|hint| hint.contains("--public")),
            "it says how to go ahead: {refusal:?}"
        );
    }

    #[test]
    fn saying_it_out_loud_is_enough() {
        assert!(refuse_without_public(a_policy_you_could_apply(), true).is_none());
        assert!(refuse_without_public(Access::Open, true).is_none());
    }

    #[test]
    fn a_hostname_you_cannot_protect_is_not_told_to_protect_it() {
        // The advice for a named tunnel cannot be followed on a quick
        // one: the hostname is Cloudflare's, and there is nothing of the
        // user's to attach a policy to. Repeating it would send somebody
        // to a dashboard page that does not apply to them.
        let refusal = refuse_without_public(Access::Open, false).expect("refused");
        let hint = refusal.hint.unwrap_or_default();

        assert!(hint.contains("no access control"), "got: {hint}");
        assert!(
            !hint.contains("policy in front"),
            "it must not advise what cannot be done: {hint}"
        );
    }

    #[test]
    fn a_guarded_tunnel_has_nothing_to_acknowledge() {
        // **Nothing produces `Managed` yet.** It is the shape the gate
        // has to have room for — a service Kobune runs can promise what a
        // CLI cannot — and this pins the branch so the promise is not
        // discovered to be unwired on the day something makes it.
        assert!(refuse_without_public(Access::Managed, false).is_none());
    }

    #[test]
    fn a_zone_a_switch_leaves_behind_is_named_while_anything_still_knows_it() {
        // **The last moment it can be said.** Switching to a provider
        // that has no use for a zone drops the domain from the record,
        // and `uninstall` reports leftovers out of that record — so
        // after this, nothing names the tunnel or the wildcard the old
        // provider left in the account, and a record pointing at a
        // deleted tunnel answers with Cloudflare's 1033 forever.
        let provider = kobune_tunnel::create(kobune_core::DEFAULT_TUNNEL_PROVIDER).expect("builds");
        let request = kobune_tunnel::TunnelRequest::new("/tmp", 0)
            .with_name("kobune")
            .with_domain(Some("example.com".into()));

        let notes = left_in_the_account(provider.as_ref(), &request, "example.com");

        assert!(
            notes.iter().any(|note| note.contains("example.com")),
            "it names the zone: {notes:?}"
        );
        assert!(
            notes
                .iter()
                .any(|note| note.contains("tunnel delete --force kobune")),
            "and what removes the tunnel: {notes:?}"
        );
        assert!(
            notes.iter().any(|note| note.contains("*.example.com")),
            "and the record no command removes: {notes:?}"
        );
    }

    #[test]
    fn a_provider_that_touched_no_account_leaves_nothing_to_say() {
        // A quick tunnel writes nothing anywhere. "It keeps what it set
        // up" about a provider that set nothing up would send somebody
        // looking through a dashboard for something that was never there.
        let provider = kobune_tunnel::create("quick").expect("builds");
        let request = kobune_tunnel::TunnelRequest::new("/tmp", 0);

        assert!(left_in_the_account(provider.as_ref(), &request, "example.com").is_empty());
    }

    fn a_record(provider: &str, domain: Option<&str>) -> TunnelRecord {
        TunnelRecord {
            provider: provider.into(),
            name: "kobune".into(),
            domain: domain.map(str::to_string),
            enabled: true,
            zone_routed: false,
        }
    }

    #[test]
    fn a_tunnel_that_cannot_run_still_replaces_the_one_that_can() {
        // The record is saved even when the provider is not installed, so
        // without this the old tunnel keeps publishing while `status` and
        // `doctor` describe the new one — "not installed" printed over an
        // environment that is still on the internet.
        assert!(replaces_what_is_running(
            Some(&a_record("cloudflare", Some("example.com"))),
            &a_record("quick", Some("example.com"))
        ));
        assert!(
            replaces_what_is_running(
                Some(&a_record("cloudflare", Some("example.com"))),
                &a_record("cloudflare", Some("elsewhere.example"))
            ),
            "a zone is as much a change of tunnel as a service is"
        );
    }

    #[test]
    fn the_same_tunnel_asked_for_again_is_left_running() {
        // A `cloudflared` that has gone from `PATH` says nothing about
        // the process still carrying the traffic, and taking a working
        // tunnel down over it would be the command doing harm.
        assert!(!replaces_what_is_running(
            Some(&a_record("cloudflare", Some("example.com"))),
            &a_record("cloudflare", Some("example.com"))
        ));
        assert!(
            !replaces_what_is_running(None, &a_record("quick", None)),
            "and a first enable has nothing to replace"
        );
    }

    #[test]
    fn the_older_records_read_as_a_sentence_on_their_own() {
        let older = vec!["*.myapp.example.com".to_string()];

        let following = older_records_note(&older, &["something the provider said".to_string()])
            .expect("there are records");
        assert!(following.starts_with("and,"), "got: {following}");

        // Nothing above it to continue: a provider this build does not
        // have describes no leftovers of its own, and "and, …" alone
        // reads as the tail of a line that was never printed.
        let alone = older_records_note(&older, &[]).expect("there are records");
        assert!(!alone.starts_with("and,"), "got: {alone}");
        assert!(alone.contains("*.myapp.example.com"), "got: {alone}");
    }

    #[test]
    fn nothing_older_is_nothing_said() {
        assert!(older_records_note(&[], &[]).is_none());
    }

    #[test]
    fn a_failure_carries_the_hint_of_the_provider_that_failed() {
        // The error itself names no provider — the same `NotInstalled`
        // comes from any of them — so being told to install the wrong
        // program is the failure this prevents.
        let err = tunnel_error(
            kobune_core::DEFAULT_TUNNEL_PROVIDER,
            TunnelError::NotLoggedIn,
        );

        assert_eq!(err.code, ErrorCode::RuntimeUnavailable);
        assert!(
            err.hint
                .as_deref()
                .is_some_and(|hint| hint.contains("cloudflared")),
            "got: {err:?}"
        );
    }

    #[test]
    fn a_provider_with_nothing_to_suggest_says_nothing() {
        // Rather than the last provider's advice, or a generic sentence
        // that fits none of them.
        let err = tunnel_error("from-the-future", TunnelError::NotLoggedIn);

        assert!(err.hint.is_none(), "got: {err:?}");
    }

    #[test]
    fn a_provider_this_build_does_not_have_is_unsupported() {
        // It reaches here from the state file, which keeps an
        // unrecognised value rather than failing to load.
        let err = tunnel_error(
            "from-the-future",
            TunnelError::Unsupported("no such tunnel provider `from-the-future`".into()),
        );

        assert_eq!(err.code, ErrorCode::Unsupported);
        assert!(err.message.contains("from-the-future"), "got: {err:?}");
    }
}
