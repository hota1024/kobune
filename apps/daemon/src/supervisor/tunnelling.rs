//! Owning the Cloudflare Tunnel: enabling it, taking it down, and
//! bringing it back after a restart.
//!
//! **One named tunnel per machine**, carrying every project, with the
//! project as a label in the hostname (`docs/DESIGN.md` §9). Everything
//! after `cloudflared tunnel login` is non-interactive and the daemon
//! does it — `tunnel create` and `tunnel route dns` run on every enable
//! and every start, with "it already exists" read as success, because a
//! flag in the state file can disagree with what Cloudflare has.

use minato_api::{ApiError, ErrorCode, Response, Target};
use minato_core::TunnelRecord;
use minato_runtime::EventSink;

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

        let record = TunnelRecord {
            name: existing
                .as_ref()
                .map(|record| record.name.clone())
                .unwrap_or_else(|| minato_tunnel::DEFAULT_TUNNEL_NAME.to_string()),
            domain,
            enabled: true,
            routed: existing.map(|record| record.routed).unwrap_or_default(),
        };

        let settings = self.tunnel_settings(&record)?;

        // Nothing to run before cloudflared is installed and logged in,
        // and login opens a browser. Report the step instead of failing:
        // the state is legitimate and the answer is a command to run.
        let readiness = minato_tunnel::readiness(&settings);
        if !readiness.is_ready() {
            return Ok(Response::Tunnel(
                tunnel::info(
                    Some(&record),
                    &self.tunnel,
                    Some(&settings),
                    &context.project,
                )
                .await,
            ));
        }

        // Every known project gets a DNS route, not just this one. The
        // tunnel is machine-wide, and a project left unrouted is silently
        // unreachable.
        let projects = self.known_projects().await?;

        events.step_started("tunnel", "starting the tunnel");
        match self.tunnel.start(settings.clone(), projects.clone()).await {
            Ok(()) => events.step_done("tunnel", "starting the tunnel"),
            Err(err) => {
                events.step_failed("tunnel", "starting the tunnel", err.to_string());
                return Err(tunnel_error(err));
            }
        }

        let mut record = record;
        record.routed.extend(projects);
        self.save_tunnel_record(Some(record.clone())).await?;

        // The routing table is rebuilt so the tunnel hostnames resolve.
        // Without this the tunnel is up and every request through it 404s
        // until something else happens to refresh.
        self.refresh(&context.project, &context.config).await?;

        Ok(Response::Tunnel(
            tunnel::info(
                Some(&record),
                &self.tunnel,
                Some(&settings),
                &context.project,
            )
            .await,
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
            tunnel::info(
                record.as_ref(),
                &self.tunnel,
                settings.as_ref(),
                &context.project,
            )
            .await,
        ))
    }
    /// Reports where the tunnel stands. Runs nothing.
    pub(super) async fn tunnel_status(&self, target: Target) -> Result<Response, ApiError> {
        let context = self.resolve_project_only(&target).await?;
        let record = self.tunnel_record().await?;

        let settings = record
            .as_ref()
            .and_then(|record| self.tunnel_settings(record).ok());

        Ok(Response::Tunnel(
            tunnel::info(
                record.as_ref(),
                &self.tunnel,
                settings.as_ref(),
                &context.project,
            )
            .await,
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

        let projects = self.known_projects().await.unwrap_or_default();

        match self.tunnel.start(settings, projects).await {
            Ok(()) => tracing::info!("tunnel restored for *.{}", record.domain),
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

        Some(minato_api::TunnelLeftover {
            domain: Some(record.domain.clone()),
            commands: vec![format!(
                "cloudflared tunnel delete --force {}",
                minato_tunnel::DEFAULT_TUNNEL_NAME
            )],
        })
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
