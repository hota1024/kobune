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

        let record = TunnelRecord {
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
            return Ok(Response::Tunnel(
                tunnel::info(Some(&record), &self.tunnel, Some(&configured)).await,
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

        let mut record = record;
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

        Ok(Response::Tunnel(
            tunnel::info_with_notes(
                Some(&record),
                &self.tunnel,
                Some(&configured),
                outcome.notes,
            )
            .await,
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

        let mut configured = match self.tunnel_configured(&record) {
            Ok(configured) => configured,
            Err(err) => {
                tracing::warn!("not starting the tunnel: {err}");
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
        if configured.provider.needs().targets {
            tracing::info!(
                "not restoring the {} tunnel: the hostnames it handed out \
                 went with it. Run `kobune tunnel enable --public` for new ones",
                configured.provider.display_name()
            );
            return;
        }

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
        // Absent when the record names a provider this build does not
        // have. There is then nothing truthful to say about an account
        // Kobune cannot describe, and the local half has gone regardless.
        let leftover = self
            .tunnel_configured(&record)
            .ok()
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

        if !older.is_empty() {
            notes.push(format!(
                "and, from before tunnel hostnames were one label, {}",
                older.join(", ")
            ));
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
