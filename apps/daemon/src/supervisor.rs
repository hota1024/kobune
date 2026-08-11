//! The lifecycle of a workspace.
//!
//! No human-facing strings go into a response. Progress goes to an
//! [`EventSink`], and how to show it is the CLI's and the GUI's own
//! decision (`docs/DESIGN.md` §3).

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use minato_api::{
    ApiError, Check, Diagnostics, EnvInfo, ErrorCode, Pong, PurgeProject, PurgeReport,
    PurgeWorkspace, Request, Response, ServiceInfo, Target, TunnelState, Typed, Window,
    WorkspaceInfo,
};
use minato_core::{
    HealthCheck, MinatoConfig, Paths, ServiceScope, ServiceState, StateStore, TunnelRecord,
    WorkspaceRecord,
};
use minato_proxy::{Activation, Route};
use minato_runtime::{EventSink, Runtime, ServiceStatus, Sizing, WorkspaceKey};

/// How the idle sweep groups services: (workspace, service).
type ServiceKeyRef = (String, String);
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

/// Where a request reads what the client types while it runs.
///
/// Only interactive `logs` reads any of it. The connection puts it on a
/// channel per request; see `server.rs`.
pub type ClientStream = tokio::sync::mpsc::UnboundedReceiver<Typed>;

use crate::env;
use crate::gateway::{BindFailure, Gateway};
use crate::idle::IdleTracker;
use crate::resolve::{self, ProjectContext, Resolved};
use crate::secrets;
use crate::spec;
use crate::tunnel::{self, TunnelHandle};

pub struct Supervisor {
    paths: Paths,
    store: StateStore,
    /// One runtime per `[runtime] default`, reused. Different projects
    /// can then run on different runtimes.
    runtimes: Mutex<HashMap<String, Arc<dyn Runtime>>>,
    /// Serialises writes to the state file.
    state_lock: Mutex<()>,
    /// Serialises `setup` across the check, the run and the record.
    ///
    /// Its own lock rather than [`Self::state_lock`]: this is held for as
    /// long as an install takes, and holding the state lock that long
    /// would stall every unrelated command.
    setup_lock: Mutex<()>,
    /// The proxy and DNS. Also where URLs come from.
    gateway: Arc<Gateway>,
    /// Last-access times, which is what scale-to-zero decides on.
    idle: IdleTracker,
    /// The Cloudflare Tunnel, when one is running.
    tunnel: Arc<TunnelHandle>,
    started_at: Instant,
    shutdown: Arc<tokio::sync::Notify>,
}

impl Supervisor {
    pub fn new(
        paths: &Paths,
        gateway: Arc<Gateway>,
        tunnel: Arc<TunnelHandle>,
        shutdown: Arc<tokio::sync::Notify>,
    ) -> Self {
        Self {
            paths: paths.clone(),
            store: StateStore::new(paths.state_file()),
            runtimes: Mutex::new(HashMap::new()),
            state_lock: Mutex::new(()),
            setup_lock: Mutex::new(()),
            gateway,
            idle: IdleTracker::new(),
            tunnel,
            started_at: Instant::now(),
            shutdown,
        }
    }

    /// Runs a request.
    ///
    /// `from_client` carries what the client sends *while* the request
    /// runs: keystrokes and window sizes. Only interactive `logs` reads
    /// any of it; every other request leaves it unread, and it is dropped
    /// when the request finishes.
    pub async fn handle(
        &self,
        request: Request,
        events: &EventSink,
        from_client: ClientStream,
    ) -> Result<Response, ApiError> {
        match request {
            Request::Ping => self.ping().await,
            Request::Shutdown => {
                self.shutdown.notify_waiters();
                Ok(Response::Empty)
            }
            Request::Purge { dry_run } => self.purge(dry_run, events).await,
            Request::Ls {
                target,
                all_projects,
            } => self.ls(target, all_projects).await,
            Request::New {
                target,
                branch,
                base,
                path,
                start,
                rebuild,
            } => {
                self.new_workspace(
                    target,
                    NewWorkspace {
                        branch,
                        base,
                        path,
                        start,
                        rebuild,
                    },
                    events,
                )
                .await
            }
            Request::Rm { target, force } => self.rm(target, force, events).await,
            Request::Up {
                target,
                services,
                rebuild,
            } => self.up(target, services, rebuild, events).await,
            Request::Down {
                target,
                services,
                all,
            } => self.down(target, services, all, events).await,
            Request::Status { target } => self.status(target).await,
            Request::Doctor { target } => self.doctor(target).await,
            Request::Logs {
                target,
                services,
                follow,
                tail,
                attach,
            } => {
                let options = minato_runtime::LogOptions { follow, tail };
                self.logs(target, services, options, attach, from_client, events)
                    .await
            }
            Request::Exec {
                target,
                service,
                command,
                fresh,
                workdir,
            } => {
                self.exec(target, service, command, fresh, workdir, events)
                    .await
            }
            Request::EnvList {
                target,
                reveal,
                service,
            } => self.env_list(target, reveal, service).await,
            Request::EnvSet {
                target,
                scope,
                key,
                value,
            } => self.env_set(target, scope, key, value).await,
            Request::EnvUnset { target, scope, key } => self.env_unset(target, scope, key).await,
            Request::TunnelEnable {
                target,
                domain,
                public,
            } => self.tunnel_enable(target, domain, public, events).await,
            Request::TunnelDisable { target } => self.tunnel_disable(target).await,
            Request::TunnelStatus { target } => self.tunnel_status(target).await,
        }
    }

    async fn ping(&self) -> Result<Response, ApiError> {
        // Every reachable runtime, not just Docker. Which one a project
        // uses is its own business, and a handshake that named Docker on a
        // machine running Apple Container was simply wrong.
        let mut reachable = Vec::new();
        for id in minato_runtime::AVAILABLE_RUNTIMES {
            if let Some(info) = self.probe_runtime(id).await {
                reachable.push(format!("{} {}", info.id, info.version));
            }
        }

        Ok(Response::Pong(Pong {
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol: minato_api::PROTOCOL_VERSION,
            runtime: if reachable.is_empty() {
                "none reachable".to_string()
            } else {
                reachable.join(", ")
            },
            uptime_secs: self.started_at.elapsed().as_secs(),
        }))
    }

    /// Probes a runtime, treating any failure as "not reachable".
    ///
    /// Both a runtime that cannot be constructed and one that cannot be
    /// reached mean the same thing to a caller, and neither is worth an
    /// error of its own.
    async fn probe_runtime(&self, id: &str) -> Option<minato_runtime::RuntimeInfo> {
        self.runtime(id).await.ok()?.probe().await.ok()
    }

    /// Diagnoses what the daemon can see.
    ///
    /// System-side settings — `/etc/resolver`, whether the CA is trusted —
    /// are hard to judge from here, so the CLI covers those. This looks at
    /// the listeners and the runtime.
    async fn doctor(&self, target: Target) -> Result<Response, ApiError> {
        let mut checks = Vec::new();

        // Read once: it decides both what the proxy checks advise and what
        // the launchd check says, and those two have to agree.
        let launchd_installed = minato_core::launchd::is_installed();

        // Which runtime this project runs on. Diagnosing a machine with no
        // project is still worth doing — that is often *why* someone runs
        // doctor — so failing to resolve falls back to the default.
        let configured = self
            .resolve_project_only(&target)
            .await
            .map(|context| context.config.runtime.default.clone())
            .unwrap_or_else(|_| "docker".to_string());

        checks.extend(self.runtime_checks(&configured).await);

        checks.push(match self.gateway.http_port() {
            Some(port) => Check::ok(
                "proxy-http",
                "HTTP proxy",
                listening_detail(port, self.gateway.http_fell_back()),
            ),
            None => {
                let failure = self.gateway.http_failure();
                Check::fail("proxy-http", "HTTP proxy", detail_for(failure)).with_fix(bind_fix(
                    failure,
                    crate::gateway::HTTP_PORT_ENV,
                    launchd_installed,
                ))
            }
        });

        // With only one address family bound, requests to the other reach
        // some different process. Passing over that silently leaves the
        // cause impossible to find.
        let missing = self.gateway.missing_families();
        if !missing.is_empty() {
            // Which proxy is short, not just which address. They bind
            // separately, so "[::1] could not be held" leaves you looking
            // at the wrong one half the time.
            let gaps: Vec<String> = missing
                .iter()
                .map(|(proxy, family)| format!("{proxy} could not hold {}", bracketed(*family)))
                .collect();

            checks.push(
                Check::fail(
                    "proxy-families",
                    "listening addresses",
                    format!(
                        "{}. *.localhost resolves to both families and clients \
                         prefer IPv6, so requests to that address reach \
                         another process",
                        gaps.join("; ")
                    ),
                )
                .with_fix(
                    "stop whatever else is on that address, or name free ports \
                     with MINATO_HTTP_PORT and MINATO_HTTPS_PORT",
                ),
            );
        }

        checks.push(match self.gateway.https_port() {
            Some(port) => Check::ok(
                "proxy-https",
                "HTTPS proxy",
                listening_detail(port, self.gateway.https_fell_back()),
            ),
            None => {
                let failure = self.gateway.https_failure();
                Check::warn(
                    "proxy-https",
                    "HTTPS proxy",
                    format!("{}; HTTP only", detail_for(failure)),
                )
                .with_fix(bind_fix(
                    failure,
                    crate::gateway::HTTPS_PORT_ENV,
                    launchd_installed,
                ))
            }
        });

        checks.push(match self.gateway.dns_port() {
            Some(port) => Check::ok("dns", "DNS server", format!("127.0.0.1:{port}")),
            None => {
                let failure = self.gateway.dns_failure();
                Check::fail(
                    "dns",
                    "DNS server",
                    format!("{}; *.localhost will not resolve", detail_for(failure)),
                )
                .with_fix(bind_fix(
                    failure,
                    crate::gateway::DNS_PORT_ENV,
                    launchd_installed,
                ))
            }
        });

        // Whether privileged ports work comes down to whether launchd
        // handed over any descriptors.
        //
        // **A job launchd has, sitting idle, is its own state**, and the one
        // `minato daemon stop` leaves behind. Telling that apart from "never
        // set up" is the difference between a fix that works and being sent
        // back to a `minato setup` that is already done.
        //
        // The plist being on disk is not enough to say which it is: one
        // copied in without a `bootstrap` behind it leaves launchd knowing
        // nothing about the job, and `kickstart` no service to name. That is
        // the install case, so `is_loaded` rather than `is_installed` — and
        // it keeps this in step with what `minato setup` offers.
        checks.push(if crate::activation::is_active() {
            Check::ok(
                "launchd",
                "launchd socket activation",
                "active (privileged ports are available)".to_string(),
            )
        } else if minato_core::launchd::is_loaded() {
            Check::warn(
                "launchd",
                "launchd socket activation",
                "inactive, though launchd has the LaunchDaemon".to_string(),
            )
            // This daemon got no descriptors from launchd, so it is not
            // launchd's — which makes it **the reason launchd's job is not
            // running**: that one stands down when it finds the socket
            // taken, and a clean exit is not restarted. Waking it without
            // this one going first only repeats that.
            .with_fix(format!(
                "this daemon was not started by launchd, so it holds the \
                 socket launchd's job wants. `minato daemon stop` hands it \
                 over; if it stays inactive, run `{}`",
                minato_core::launchd::kickstart_command()
            ))
        } else {
            Check::warn(
                "launchd",
                "launchd socket activation",
                "inactive; 80 and 443 are out, so it listens elsewhere".to_string(),
            )
            .with_fix("follow `minato setup` to install the LaunchDaemon")
        });

        checks.push(match self.gateway.ca_path() {
            Some(path) => Check::ok("ca", "local CA", path.display().to_string()),
            None => Check::warn("ca", "local CA", "not generated".to_string()),
        });

        // Only worth reporting once a tunnel has been set up. An unused
        // feature showing up as a warning on every `doctor` run trains
        // people to skim past the output.
        if let Some(check) = self.tunnel_check().await {
            checks.push(check);
        }

        Ok(Response::Diagnostics(Diagnostics::new(checks)))
    }

    /// The runtime for an identifier, built once and reused.
    async fn runtime(&self, id: &str) -> Result<Arc<dyn Runtime>, ApiError> {
        let mut runtimes = self.runtimes.lock().await;

        if let Some(runtime) = runtimes.get(id) {
            return Ok(runtime.clone());
        }

        let runtime: Arc<dyn Runtime> = Arc::from(minato_runtime::create(id)?);
        runtimes.insert(id.to_string(), runtime.clone());
        Ok(runtime)
    }

    /// Resolves the project and workspace from `cwd`.
    async fn resolve(&self, target: &Target) -> Result<Resolved, ApiError> {
        let _guard = self.state_lock.lock().await;

        self.store
            .update(|state| {
                let context = resolve::resolve_project(target, state)?;
                context.resolve_workspace(target, state)
            })
            .map_err(ApiError::from)
    }

    /// Resolves the project alone, for operations whose workspace does
    /// not exist yet.
    async fn resolve_project_only(&self, target: &Target) -> Result<ProjectContext, ApiError> {
        let _guard = self.state_lock.lock().await;

        self.store
            .update(|state| resolve::resolve_project(target, state))
            .map_err(ApiError::from)
    }

    /// Re-reads the runtime's state and rebuilds the routing table.
    ///
    /// Everything is swapped wholesale rather than tracked change by
    /// change. That matches treating the runtime's labels as the source of
    /// truth, and nothing slips through.
    async fn refresh(
        &self,
        project: &str,
        config: &MinatoConfig,
    ) -> Result<Vec<ServiceStatus>, ApiError> {
        let runtime = self.runtime(&config.runtime.default).await?;
        let statuses = runtime.list_project(project).await?;

        let records = self.workspace_records(project).await?;
        let entries = route_entries(
            config,
            project,
            &records,
            &statuses,
            self.tunnel.domain().as_deref(),
        );

        // Give a baseline to hosts that are running with no record — after
        // a daemon restart, say. Without one they never look idle and
        // never stop.
        for (host, route) in &entries {
            if route.is_running() && self.idle.idle_for(host).is_none() {
                self.idle.touch(host);
            }
        }

        self.gateway.routes().replace_project(project, entries);

        Ok(statuses)
    }

    /// [`Self::refresh`], plus the readiness the caller is about to show.
    ///
    /// **Kept off `refresh` itself.** That one also runs where nobody is
    /// reading the answer: on the proxy's cold-start path, where it would
    /// put a probe of every container in the project in front of the
    /// request that woke one, and at daemon boot, where it would delay the
    /// first command by a probe per registered project. Neither publishes
    /// a state to anyone.
    async fn refresh_for_display(
        &self,
        project: &str,
        config: &MinatoConfig,
    ) -> Result<Vec<ServiceStatus>, ApiError> {
        let mut statuses = self.refresh(project, config).await?;
        settle_readiness(config, &mut statuses).await;
        Ok(statuses)
    }

    /// Reads logs and emits them as events.
    ///
    /// Under `follow` this streams until the client disconnects. The
    /// daemon stops the moment a write fails, so nothing has to cancel it
    /// explicitly.
    async fn logs(
        &self,
        target: Target,
        services: Vec<String>,
        options: minato_runtime::LogOptions,
        attach: Option<Window>,
        from_client: ClientStream,
        events: &EventSink,
    ) -> Result<Response, ApiError> {
        let resolved = self.resolve(&target).await?;
        let runtime = self.runtime(&resolved.config.runtime.default).await?;

        let targets: Vec<String> = if services.is_empty() {
            resolved.config.services.keys().cloned().collect()
        } else {
            validate_service_names(&resolved.config, &services)?;
            services
        };

        let workspace_key = WorkspaceKey::new(&resolved.project, &resolved.workspace.label);
        let shared_key = WorkspaceKey::shared(&resolved.project);

        // The client offered its terminal. Whether that is taken up
        // depends on there being exactly one service and that service
        // having a terminal to offer back; when it is not, the reason is
        // said out loud and the plain log stream follows.
        if let Some(window) = attach {
            match self.attachable(&resolved, &targets) {
                Ok((name, scope)) => {
                    let key = match scope {
                        ServiceScope::Workspace => workspace_key.service(&name),
                        ServiceScope::Project => shared_key.service(&name),
                    };

                    match self
                        .attach(runtime.as_ref(), &key, &name, window, from_client, events)
                        .await
                    {
                        Ok(response) => return Ok(response),
                        // **A warning, like every other way of declining.**
                        // The logs were asked for as well, and they can
                        // still be given: failing outright would answer a
                        // request for `logs` with nothing but the reason
                        // the terminal was unavailable.
                        Err(err) => events.warn(err.message),
                    }
                }
                Err(reason) => events.warn(reason),
            }
        }

        let mut streams = Vec::new();
        for name in &targets {
            let service_config = resolved.config.service(name).map_err(ApiError::from)?;
            let key = match service_config.scope {
                ServiceScope::Workspace => workspace_key.service(name),
                ServiceScope::Project => shared_key.service(name),
            };

            match runtime.logs(&key, options.clone()).await {
                Ok(stream) => streams.push((name.clone(), stream)),
                Err(err) => {
                    // One unreadable service does not hide the rest.
                    // Failing outright would leave nobody able to tell
                    // which one is down.
                    events.warn(format!("cannot read {name}'s logs: {err}"));
                }
            }
        }

        if streams.is_empty() {
            return Err(
                ApiError::not_found("no service has readable logs".to_string())
                    .with_hint("check what is running with `minato status`"),
            );
        }

        // Several services' logs merge into one stream. Which line came
        // from where is in Event::Output's service field.
        let mut merged = futures::stream::select_all(streams.into_iter().map(|(name, stream)| {
            Box::pin(stream.map(move |line| (name.clone(), line)))
                as futures::stream::BoxStream<'static, (String, minato_runtime::LogLine)>
        }));

        while let Some((service, entry)) = merged.next().await {
            events.output(Some(service), entry.stream, entry.line);
        }

        Ok(Response::Empty)
    }

    /// Which service `logs` may hand the client's terminal to.
    ///
    /// The `Err` is what to tell the person instead — a warning, not a
    /// failure. They asked to read logs and will get logs; what they will
    /// not get is to type, and being told why beats a keyboard that
    /// quietly does nothing.
    fn attachable(
        &self,
        resolved: &Resolved,
        targets: &[String],
    ) -> Result<(String, ServiceScope), String> {
        let [name] = targets else {
            return Err("typing needs one service to type at. Name one, as in \
                 `minato logs -f web`"
                .to_string());
        };

        let service = resolved
            .config
            .service(name)
            .map_err(|err| err.to_string())?;

        if !service.tty {
            return Err(format!(
                "{name} has no terminal, so it cannot take input. Add \
                 `tty = true` under [services.{name}] in minato.toml, then \
                 `minato down && minato up`"
            ));
        }

        // The scope comes back with the name: the caller needs it to build
        // the key, and looking the service up again to get it would mean
        // handling a failure that has just been ruled out.
        Ok((name.clone(), service.scope))
    }

    /// Hands the client's terminal to a running service, both ways.
    ///
    /// Ends when the service's terminal closes — the container stopped —
    /// or when the client goes away. Neither is a failure: leaving is how
    /// this is meant to end.
    async fn attach(
        &self,
        runtime: &dyn Runtime,
        key: &minato_runtime::ServiceKey,
        service: &str,
        window: Window,
        mut from_client: ClientStream,
        events: &EventSink,
    ) -> Result<Response, ApiError> {
        let minato_runtime::Attachment {
            mut output,
            mut input,
            sizing,
        } = runtime.attach(key).await?;

        match &sizing {
            // Sized before the first frame, rather than left to a resize
            // that arrives once the program has already drawn.
            Sizing::Follows(terminal) => {
                if let Err(err) = terminal.resize(window).await {
                    events.debug(format!("cannot size {service}'s terminal: {err}"));
                }
            }
            // **Said before the screen is handed over.** After that the
            // program owns the display, and a warning printed into it
            // would be drawn over and lost.
            Sizing::Fixed(fixed) if *fixed != window => events.warn(format!(
                "this runtime fixes a container's terminal at {fixed} when \
                 the service starts and cannot resize it, so {service} will \
                 draw to {fixed} rather than to your {window} window"
            )),
            Sizing::Fixed(_) => {}
        }

        events.attached(service);

        // **Being typed at counts as being used.** The idle sweep reads
        // times, and someone watching a task runner produces none — so
        // without this claim, scale-to-zero would stop the service out
        // from under an open session after `idle_timeout` of deliberate
        // use. It is given back when this returns.
        let _session = self.idle.begin_use(key.to_string());

        loop {
            tokio::select! {
                chunk = output.next() => match chunk {
                    Some(bytes) => events.bytes(&bytes),
                    // The container's terminal closed. The service
                    // stopped, or was stopped.
                    None => break,
                },
                message = from_client.recv() => match message {
                    Some(Typed::Keys(keys)) => {
                        if input.write_all(&keys).await.is_err() {
                            break;
                        }
                        // Flushed per keystroke. A terminal that answered
                        // in batches would not be one.
                        let _ = input.flush().await;
                    }
                    Some(Typed::Resize(window)) => {
                        if let Sizing::Follows(terminal) = &sizing
                            && let Err(err) = terminal.resize(window).await
                        {
                            events.debug(format!("cannot resize {service}'s terminal: {err}"));
                        }
                    }
                    // The client hung up.
                    None => break,
                },
            }
        }

        Ok(Response::Empty)
    }

    /// Runs a command inside a container.
    async fn exec(
        &self,
        target: Target,
        service: String,
        command: Vec<String>,
        fresh: bool,
        workdir: Option<String>,
        events: &EventSink,
    ) -> Result<Response, ApiError> {
        if command.is_empty() {
            return Err(ApiError::new(
                ErrorCode::InvalidConfig,
                "no command was given".to_string(),
            ));
        }

        let resolved = self.resolve(&target).await?;
        validate_service_names(&resolved.config, std::slice::from_ref(&service))?;

        let service_config = resolved.config.service(&service).map_err(ApiError::from)?;
        let key = match service_config.scope {
            ServiceScope::Workspace => {
                WorkspaceKey::new(&resolved.project, &resolved.workspace.label).service(&service)
            }
            ServiceScope::Project => WorkspaceKey::shared(&resolved.project).service(&service),
        };

        let runtime = self.runtime(&resolved.config.runtime.default).await?;
        let options = minato_runtime::ExecOptions { workdir };

        let outcome = if fresh {
            let spec = self.service_spec(&resolved, &service, events).await?;

            // The image may never have been pulled — `--fresh` is at its
            // most useful before a service has ever come up cleanly — so
            // the same groundwork `up` does runs first.
            let workspace = minato_runtime::WorkspaceSpec {
                key: spec.key.workspace.clone(),
                worktree_path: resolved.workspace.path.clone(),
                services: vec![spec.clone()],
            };
            runtime.prepare(&workspace, false, events).await?;

            runtime
                .exec_fresh(&spec, &command, &options, events)
                .await?
        } else {
            runtime.exec(&key, &command, &options, events).await?
        };

        Ok(Response::Exec {
            exit_code: outcome.exit_code,
        })
    }

    /// The spec for one service, as `up` would build it.
    async fn service_spec(
        &self,
        resolved: &Resolved,
        service: &str,
        events: &EventSink,
    ) -> Result<minato_runtime::ServiceSpec, ApiError> {
        let envs = self
            .workspace_envs(
                &resolved.config,
                &resolved.project,
                &resolved.workspace,
                &resolved.repo.main_root,
                events,
            )
            .await?;

        let workspace_spec = spec::build_workspace_spec(
            &resolved.config,
            &resolved.project,
            &resolved.workspace.label,
            &resolved.workspace.path,
            &envs,
        )?;

        workspace_spec
            .service(service)
            .cloned()
            .ok_or_else(|| ApiError::not_found(format!("no service named `{service}`")))
    }

    /// Shows the environment, layer by layer.
    ///
    /// **Each value says which layer defined it.** With three layers, not
    /// seeing that an unintended one is winning makes the cause impossible
    /// to find.
    async fn env_list(
        &self,
        target: Target,
        reveal: bool,
        service: Option<String>,
    ) -> Result<Response, ApiError> {
        let resolved = self.resolve(&target).await?;

        // Named: what that container is given, its own `env` included.
        // Unnamed: only what every service shares.
        //
        // **Not "whichever service came first".** That is what this used to
        // do, and it showed one service's own variables as if they were
        // everyone's.
        if let Some(service) = &service {
            validate_service_names(&resolved.config, std::slice::from_ref(service))?;
        }

        let layers = env::layers_for_service(
            &resolved.config,
            &resolved.project,
            &resolved.workspace,
            &resolved.repo.main_root,
            service.as_deref(),
            &self.paths,
            &self.gateway,
        )
        .map_err(|err| ApiError::new(ErrorCode::InvalidConfig, err.to_string()))?;

        // **A listing that cannot settle still lists.** This is the tool
        // someone reaches for to find the value that will not settle, and
        // one bad `${...}` taking the whole listing with it leaves them
        // with the error alone and nowhere to look.
        let (settled, unresolved) = match layers.resolve() {
            Ok(settled) => (settled, None),
            Err(err) => (
                layers.unexpanded(),
                Some(env::listing_note(
                    &err,
                    service.as_deref(),
                    &resolved.config,
                )),
            ),
        };

        let entries = settled
            .into_iter()
            .map(|entry| {
                let secret = entry.secret_ref();

                // Injected values are Minato's own and hold no secrets.
                // Checking a URL is common, so they stay visible.
                let injected = entry.scope == minato_core::EnvScope::Injected;

                EnvInfo {
                    key: entry.key,
                    value: if reveal || injected || secret.is_some() {
                        // A secret stays a reference even under --reveal.
                        // Showing the value would mean resolving it, and
                        // that only happens at start.
                        entry.raw.clone()
                    } else {
                        minato_core::env::mask(&entry.raw)
                    },
                    scope: entry.scope,
                    secret: secret.is_some(),
                    source: secret.map(|reference| reference.describe()),
                }
            })
            .collect();

        Ok(Response::Env {
            entries,
            service,
            unresolved,
        })
    }

    async fn env_set(
        &self,
        target: Target,
        scope: minato_core::EnvScope,
        key: String,
        value: String,
    ) -> Result<Response, ApiError> {
        if !minato_core::env::is_valid_key(&key) {
            return Err(ApiError::new(
                ErrorCode::InvalidConfig,
                format!("`{key}` is not a valid environment variable name"),
            )
            .with_hint("letters, digits and underscores only, and not starting with a digit"));
        }

        let path = self.env_file_path(&target, scope).await?;
        let current = read_or_empty(&path)?;

        minato_core::env::write_file(&path, &minato_core::env::upsert(&current, &key, &value))
            .map_err(|err| ApiError::internal(err.to_string()))?;

        written(key, self.env_list(target, false, None).await)
    }

    async fn env_unset(
        &self,
        target: Target,
        scope: minato_core::EnvScope,
        key: String,
    ) -> Result<Response, ApiError> {
        let path = self.env_file_path(&target, scope).await?;
        let current = read_or_empty(&path)?;

        minato_core::env::write_file(&path, &minato_core::env::remove(&current, &key))
            .map_err(|err| ApiError::internal(err.to_string()))?;

        written(key, self.env_list(target, false, None).await)
    }

    /// Where a layer's file lives.
    async fn env_file_path(
        &self,
        target: &Target,
        scope: minato_core::EnvScope,
    ) -> Result<PathBuf, ApiError> {
        if !scope.is_writable() {
            return Err(ApiError::new(
                ErrorCode::InvalidConfig,
                format!("{} values cannot be written to a file", scope.label()),
            ));
        }

        if scope == minato_core::EnvScope::Global {
            return Ok(self.paths.root().join(minato_core::env::GLOBAL_ENV_FILE));
        }

        let resolved = self.resolve(target).await?;

        Ok(match scope {
            minato_core::EnvScope::Project => {
                minato_core::env::project_env_path(&resolved.repo.main_root)
            }
            _ => minato_core::env::workspace_env_path(&resolved.workspace.path),
        })
    }

    /// Settles the environment a service receives.
    ///
    /// Stacks the layers and resolves secret references. **A resolved
    /// value never touches disk**, here or anywhere after; it exists only
    /// to be handed to the container.
    async fn service_env(
        &self,
        config: &MinatoConfig,
        project: &str,
        record: &WorkspaceRecord,
        project_root: &std::path::Path,
        service: &str,
        events: &EventSink,
    ) -> Result<BTreeMap<String, String>, ApiError> {
        let layers = env::layers_for_service(
            config,
            project,
            record,
            project_root,
            Some(service),
            &self.paths,
            &self.gateway,
        )
        .map_err(|err| ApiError::new(ErrorCode::InvalidConfig, err.to_string()))?;

        let entries = layers.resolve().map_err(env::resolution_error)?;

        // `$NAME` is passed through as written — right for a value on its
        // way to a shell, a mistake everywhere else. Saying so where the
        // name is one Minato has costs a line and saves an afternoon:
        // otherwise a directory called `$MINATO_CACHE_DIR` appears in the
        // worktree and nothing connects it back to here.
        //
        // **Read from the values as written**, not from the settled ones:
        // by then `$$NAME` has become `$NAME`, and a reference has carried
        // one value's mistake into every value built out of it.
        let written = layers.unexpanded();
        for entry in &written {
            for name in minato_core::env::bare_references(&entry.raw)
                .into_iter()
                .filter(|name| written.iter().any(|other| other.key == *name))
            {
                let message = format!(
                    "{}: {} contains ${name}, which is not expanded. Write ${{{name}}} to refer to it",
                    service, entry.key
                );
                events.warn(message.clone());
                tracing::warn!("{message}");
            }
        }

        // Written before the service starts, and from the same values it
        // is about to be given: a file that disagreed with the process's
        // own environment would be worse than no file.
        if let Some(relative) = &config.service(service).map_err(ApiError::from)?.env_file {
            let note = format!("service: {service}  workspace: {}", record.label);
            let contents = minato_core::env::render(&entries, &note);

            if let Some(path) = env::write_env_file(&record.path, relative, &contents)? {
                tracing::debug!("{service}: wrote {}", path.display());
            }
        }

        // Split references from plain values.
        let mut values = BTreeMap::new();
        let mut references = Vec::new();

        for entry in entries {
            match entry.secret_ref() {
                Some(reference) => references.push((entry.key, reference)),
                None => {
                    values.insert(entry.key, entry.raw);
                }
            }
        }

        if references.is_empty() {
            return Ok(values);
        }

        let resolved = secrets::resolve(&references).await;
        values.extend(resolved.values);

        // What did not resolve is dropped, but never quietly. The worst
        // outcome is nobody noticing and wondering why authentication
        // keeps failing.
        for (key, reason) in resolved.failures {
            events.warn(format!("cannot resolve the secret for {key}: {reason}"));
            tracing::warn!("{service}: cannot resolve the secret for {key}: {reason}");
        }

        Ok(values)
    }

    /// The environments for every service in a workspace.
    async fn workspace_envs(
        &self,
        config: &MinatoConfig,
        project: &str,
        record: &WorkspaceRecord,
        project_root: &std::path::Path,
        events: &EventSink,
    ) -> Result<BTreeMap<String, BTreeMap<String, String>>, ApiError> {
        let mut envs = BTreeMap::new();

        for name in config.services.keys() {
            let values = self
                .service_env(config, project, record, project_root, name, events)
                .await?;
            envs.insert(name.clone(), values);
        }

        Ok(envs)
    }

    /// Sets up the Cloudflare Tunnel and starts it.
    ///
    /// Idempotent: creating the tunnel and routing DNS both treat "it
    /// already exists" as success, so this is the same call whether the
    /// machine has been set up before or not.
    async fn tunnel_enable(
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
    async fn tunnel_disable(&self, target: Target) -> Result<Response, ApiError> {
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
    async fn tunnel_status(&self, target: Target) -> Result<Response, ApiError> {
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

    /// Every project the state store knows about.
    pub async fn known_projects(&self) -> Result<Vec<String>, ApiError> {
        let _guard = self.state_lock.lock().await;
        let state = self.store.load().map_err(ApiError::from)?;
        Ok(state.projects.keys().cloned().collect())
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

    /// Checks the configured runtime, and mentions the alternatives.
    ///
    /// The configured one is the only one that can fail: an unreachable
    /// Docker on a machine that runs Apple Container is not a problem, and
    /// reporting it as one trains people to skim past the output. The
    /// others appear only when reachable, so `[runtime] default` can be
    /// switched to something known to work.
    async fn runtime_checks(&self, configured: &str) -> Vec<Check> {
        let title = "container runtime";
        let mut checks = Vec::new();

        checks.push(match self.runtime(configured).await {
            Ok(runtime) => match runtime.probe().await {
                Ok(info) => Check::ok("runtime", title, format!("{} {}", info.id, info.version)),
                Err(err) => Check::fail("runtime", title, err.to_string())
                    .with_fix(minato_runtime::start_hint(configured)),
            },
            // An unknown identifier in `[runtime] default` is a
            // configuration mistake, not an unreachable runtime.
            Err(err) => Check::fail("runtime", title, err.to_string()).with_fix(format!(
                "set [runtime] default to one of: {}",
                minato_runtime::AVAILABLE_RUNTIMES.join(", ")
            )),
        });

        for id in minato_runtime::AVAILABLE_RUNTIMES {
            if *id == configured {
                continue;
            }

            if let Some(info) = self.probe_runtime(id).await {
                checks.push(Check::ok(
                    "runtime-available",
                    format!("{} (available)", minato_runtime::display_name(id)),
                    format!("{} {}", info.id, info.version),
                ));
            }
        }

        checks
    }

    /// Diagnoses the tunnel, or nothing when there is none to diagnose.
    async fn tunnel_check(&self) -> Option<Check> {
        let record = self.tunnel_record().await.ok().flatten()?;

        let settings = self.tunnel_settings(&record).ok();
        let info = tunnel::info(Some(&record), &self.tunnel, settings.as_ref(), "").await;
        let title = "Cloudflare Tunnel";
        let domain = record.domain.clone();

        Some(match info.state {
            TunnelState::Running => Check::ok("tunnel", title, format!("running for *.{domain}")),
            TunnelState::Disabled => Check::ok("tunnel", title, "disabled".to_string()),
            TunnelState::NotInstalled => {
                Check::fail("tunnel", title, "cloudflared is not installed".to_string())
                    .with_fix("brew install cloudflared")
            }
            TunnelState::NeedsLogin => {
                Check::fail("tunnel", title, "cloudflared is not logged in".to_string())
                    .with_fix("cloudflared tunnel login")
            }
            // Enabled but not up. Everything published through it is
            // unreachable, and nothing local would show that.
            TunnelState::Stopped => Check::fail(
                "tunnel",
                title,
                format!("enabled for *.{domain}, but not running"),
            )
            .with_fix("run `minato tunnel enable --public`, or `minato tunnel status` for why"),
        })
    }

    /// Rebuilds every project's routing table at daemon start.
    ///
    /// The table lives in memory, so a restart leaves it empty and every
    /// URL 404s until some command happens to call [`Self::refresh`].
    /// Locally that self-corrects the first time anyone runs `status`; a
    /// reviewer following a tunnel link has no such move, and scale-to-
    /// zero cannot rescue them because the route is not registered for a
    /// request to wake.
    ///
    /// A project that cannot be refreshed is skipped rather than fatal:
    /// its `minato.toml` may have moved, or the runtime may be down, and
    /// neither is a reason to take the daemon with it.
    pub async fn restore_routes(&self) {
        let projects = match self.known_projects().await {
            Ok(projects) => projects,
            Err(err) => {
                tracing::warn!("cannot read the registered projects: {err}");
                return;
            }
        };

        for project in projects {
            let config = match self.project_config(&project).await {
                Ok(config) => config,
                Err(err) => {
                    tracing::debug!("not restoring routes for {project}: {err}");
                    continue;
                }
            };

            match self.refresh(&project, &config).await {
                Ok(_) => tracing::debug!("restored routes for {project}"),
                Err(err) => tracing::warn!("cannot restore routes for {project}: {err}"),
            }
        }
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

    /// Records an access. The proxy calls this on every request.
    pub fn touch(&self, host: &str) {
        self.idle.touch(host);
    }

    /// Wakes a stopped service.
    ///
    /// Not ready within `wait` comes back as [`Activation::Starting`], but
    /// **the start carries on**. A caller that waits again gets through.
    pub async fn activate(&self, host: &str, wait: Duration) -> Activation {
        let Some(route) = self.gateway.routes().get(host) else {
            return Activation::Unknown;
        };

        if let Some(endpoint) = route.endpoint {
            self.idle.touch(host);
            return Activation::Ready(endpoint);
        }

        // However many requests arrive for one host at once, it starts
        // once. Whoever loses the claim waits on the start already
        // running.
        match self.idle.begin_start(host) {
            Some(guard) => {
                let outcome = self.start_for_host(host, &route).await;
                drop(guard);

                match outcome {
                    Ok(Some(endpoint)) => {
                        self.idle.touch(host);
                        Activation::Ready(endpoint)
                    }
                    // Started, but with nowhere to forward to — no
                    // published port, for instance.
                    Ok(None) => Activation::Starting,
                    Err(err) => Activation::Failed(err.message),
                }
            }
            None => self.await_route(host, wait).await,
        }
    }

    /// Waits for an endpoint to appear on the route. Used when another
    /// start is already under way.
    async fn await_route(&self, host: &str, wait: Duration) -> Activation {
        let deadline = tokio::time::Instant::now() + wait;

        loop {
            if let Some(route) = self.gateway.routes().get(host)
                && let Some(endpoint) = route.endpoint
            {
                self.idle.touch(host);
                return Activation::Ready(endpoint);
            }

            if tokio::time::Instant::now() >= deadline {
                return Activation::Starting;
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Starts the one service behind a host.
    async fn start_for_host(
        &self,
        host: &str,
        route: &Route,
    ) -> Result<Option<SocketAddr>, ApiError> {
        let (config, record) = self.locate(&route.project, &route.workspace).await?;

        let service_config = config.service(&route.service).map_err(ApiError::from)?;
        let events = EventSink::discard();

        let project_root = self.project_root(&route.project).await?;
        let service_env = self
            .service_env(
                &config,
                &route.project,
                &record,
                &project_root,
                &route.service,
                &events,
            )
            .await?;

        let service_spec = spec::build_service_spec(
            service_config,
            &route.service,
            &route.project,
            &record.label,
            &record.path,
            service_env,
            config.services.keys().cloned().collect(),
        )?;

        let runtime = self.runtime(&config.runtime.default).await?;

        // start fails without the image, so prepare it as a service of
        // one.
        let workspace_spec = minato_runtime::WorkspaceSpec {
            key: WorkspaceKey::new(&route.project, &record.label),
            worktree_path: record.path.clone(),
            services: vec![service_spec.clone()],
        };
        // Never a forced rebuild: this sits in the path of the request
        // that woke the service, and the fingerprint in the tag already
        // means an existing image was built from these inputs.
        runtime.prepare(&workspace_spec, false, &events).await?;

        tracing::info!("a request to {host} is starting {}", route.service);
        let running = runtime.start(&service_spec, &events).await?;

        self.refresh(&route.project, &config).await?;
        Ok(running.endpoint)
    }

    /// The configuration and registration for a project and workspace
    /// label.
    async fn locate(
        &self,
        project: &str,
        workspace: &str,
    ) -> Result<(MinatoConfig, WorkspaceRecord), ApiError> {
        let record = {
            let _guard = self.state_lock.lock().await;
            let state = self.store.load().map_err(ApiError::from)?;

            state
                .project(project)
                .and_then(|p| p.workspace(workspace))
                .cloned()
                .ok_or_else(|| {
                    ApiError::not_found(format!("workspace `{workspace}` is not registered"))
                })?
        };

        let (_, config) = MinatoConfig::find(&record.path).map_err(ApiError::from)?;
        Ok((config, record))
    }

    /// The project root as recorded in the state store.
    async fn project_root(&self, project: &str) -> Result<PathBuf, ApiError> {
        let _guard = self.state_lock.lock().await;
        let state = self.store.load().map_err(ApiError::from)?;

        state
            .project(project)
            .map(|record| record.root.clone())
            .ok_or_else(|| ApiError::not_found(format!("project `{project}` is not registered")))
    }

    /// Reads a project's configuration from the root in the state store.
    async fn project_config(&self, project: &str) -> Result<MinatoConfig, ApiError> {
        let root = {
            let _guard = self.state_lock.lock().await;
            let state = self.store.load().map_err(ApiError::from)?;

            state
                .project(project)
                .map(|record| record.root.clone())
                .ok_or_else(|| {
                    ApiError::not_found(format!("project `{project}` is not registered"))
                })?
        };

        let (_, config) = MinatoConfig::find(&root).map_err(ApiError::from)?;
        Ok(config)
    }

    /// Stops idle services. Called on a timer by the sweeper.
    ///
    /// Returns how many were stopped.
    pub async fn sweep_idle(&self) -> usize {
        let snapshot = self.gateway.routes().snapshot();
        if snapshot.is_empty() {
            return 0;
        }

        let mut projects: BTreeMap<String, Vec<(String, Route)>> = BTreeMap::new();
        for (host, route) in snapshot {
            projects
                .entry(route.project.clone())
                .or_default()
                .push((host, route));
        }

        let mut stopped = 0;
        for (project, routes) in projects {
            match self.sweep_project(&project, &routes).await {
                Ok(count) => stopped += count,
                Err(err) => tracing::debug!("cannot sweep {project} for idle services: {err}"),
            }
        }

        stopped
    }

    async fn sweep_project(
        &self,
        project: &str,
        routes: &[(String, Route)],
    ) -> Result<usize, ApiError> {
        let config = self.project_config(project).await?;
        let runtime = self.runtime(&config.runtime.default).await?;
        let events = EventSink::discard();

        // A shared service is referenced from several workspaces, and one
        // of them still using it is enough to keep it up, so the decision
        // is made per service.
        let mut by_service: BTreeMap<ServiceKeyRef, Vec<&(String, Route)>> = BTreeMap::new();
        for entry in routes {
            let (_, route) = entry;
            if !route.is_running() {
                continue;
            }

            let Ok(service_config) = config.service(&route.service) else {
                continue;
            };

            let key = match service_config.scope {
                ServiceScope::Workspace => (route.workspace.clone(), route.service.clone()),
                // Under scope = project, the workspace is ignored and
                // everything folds into one.
                ServiceScope::Project => (String::new(), route.service.clone()),
            };

            by_service.entry(key).or_default().push(entry);
        }

        let mut stopped = 0;
        for ((workspace, service), entries) in by_service {
            let Ok(service_config) = config.service(&service) else {
                continue;
            };
            let timeout = service_config.idle_timeout();

            // One live host referencing it is enough to keep it up.
            let all_idle = entries
                .iter()
                .all(|(host, _)| self.idle.idle_for(host).is_some_and(|idle| idle >= timeout));

            if !all_idle {
                continue;
            }

            let service_key = match service_config.scope {
                ServiceScope::Workspace => WorkspaceKey::new(project, &workspace).service(&service),
                ServiceScope::Project => WorkspaceKey::shared(project).service(&service),
            };

            // Idle by the clock, but somebody is sitting at its terminal.
            // Attaching sends no requests, so this is the only trace an
            // open session leaves.
            if self.idle.is_in_use(&service_key.to_string()) {
                continue;
            }

            tracing::info!(
                "stopping {service_key} (no access for {})",
                humantime::format_duration(timeout)
            );

            if let Err(err) = runtime.stop(&service_key, &events).await {
                tracing::warn!("cannot stop {service_key}: {err}");
                continue;
            }

            for (host, _) in entries {
                self.idle.forget(host);
            }
            stopped += 1;
        }

        if stopped > 0 {
            self.refresh(project, &config).await?;
        }

        Ok(stopped)
    }

    /// Every other project's workspaces.
    ///
    /// A project whose repository has moved or gone is skipped rather than
    /// failing the listing: `ls --all-projects` is how someone finds out
    /// what is registered, so it is the wrong moment to refuse to answer.
    /// The reason goes to the log.
    async fn other_projects(&self, current: &str) -> Vec<minato_api::WorkspaceInfo> {
        let projects = match self.known_projects().await {
            Ok(projects) => projects,
            Err(err) => {
                tracing::warn!("cannot read the registered projects: {err}");
                return Vec::new();
            }
        };

        let mut workspaces = Vec::new();

        for project in projects {
            if project == current {
                continue;
            }

            match self.project_workspaces(&project).await {
                Ok(found) => workspaces.extend(found),
                Err(err) => tracing::debug!("skipping {project} in the listing: {err}"),
            }
        }

        workspaces
    }

    /// One project's workspaces, read through its own configuration.
    async fn project_workspaces(
        &self,
        project: &str,
    ) -> Result<Vec<minato_api::WorkspaceInfo>, ApiError> {
        let config = self.project_config(project).await?;
        let records = self.workspace_records(project).await?;

        // Registered worktrees only. Finding unregistered ones would mean
        // opening someone else's repository, and a worktree nobody has run
        // a command in is not one this daemon manages.
        let statuses = self.refresh_for_display(project, &config).await?;

        // Main first, matching how the current project is listed. Records
        // come back keyed by label, which would put `feature-x` above
        // `main` and make one project read differently from the next.
        let mut records = records;
        records.sort_by_key(|record| (!record.is_main, record.label.clone()));

        Ok(records
            .iter()
            .map(|record| self.build_workspace_info(&config, project, record, &statuses))
            .collect())
    }

    /// The workspaces registered in the state store.
    async fn workspace_records(&self, project: &str) -> Result<Vec<WorkspaceRecord>, ApiError> {
        let _guard = self.state_lock.lock().await;

        let state = self.store.load().map_err(ApiError::from)?;
        Ok(state
            .project(project)
            .map(|record| record.workspaces.values().cloned().collect())
            .unwrap_or_default())
    }

    async fn ls(&self, target: Target, all_projects: bool) -> Result<Response, ApiError> {
        let context = self.resolve_project_only(&target).await?;

        // Both registered and unregistered worktrees show up, so nothing
        // is in `git worktree list` but missing from `minato ls`.
        let worktrees = context.repo.worktrees().map_err(ApiError::from)?;

        let records = {
            let _guard = self.state_lock.lock().await;
            self.store
                .update(|state| {
                    let mut records = Vec::new();
                    for worktree in &worktrees {
                        if worktree.bare {
                            continue;
                        }
                        records.push(context.ensure_registered(worktree, state)?);
                    }
                    Ok(records)
                })
                .map_err(ApiError::from)?
        };

        // Ask the runtime once, then sort the answer by workspace.
        let statuses = self
            .refresh_for_display(&context.project, &context.config)
            .await?;

        let mut workspaces = Vec::with_capacity(records.len());
        for record in records {
            workspaces.push(self.build_workspace_info(
                &context.config,
                &context.project,
                &record,
                &statuses,
            ));
        }

        if all_projects {
            workspaces.extend(self.other_projects(&context.project).await);
        }

        Ok(Response::Workspaces { workspaces })
    }

    async fn status(&self, target: Target) -> Result<Response, ApiError> {
        let resolved = self.resolve(&target).await?;
        let statuses = self
            .refresh_for_display(&resolved.project, &resolved.config)
            .await?;

        Ok(Response::Workspace {
            workspace: self.build_workspace_info(
                &resolved.config,
                &resolved.project,
                &resolved.workspace,
                &statuses,
            ),
        })
    }

    async fn new_workspace(
        &self,
        target: Target,
        options: NewWorkspace,
        events: &EventSink,
    ) -> Result<Response, ApiError> {
        let NewWorkspace {
            branch,
            base,
            path,
            start,
            rebuild,
        } = options;

        let context = self.resolve_project_only(&target).await?;

        let worktree_path =
            path.unwrap_or_else(|| default_worktree_path(&context.repo.main_root, &branch));

        if worktree_path.exists() {
            return Err(ApiError::new(
                ErrorCode::AlreadyExists,
                format!("{} already exists", worktree_path.display()),
            )
            .with_hint("name another path with --path, or remove the directory"));
        }

        events.step_started("worktree", format!("creating worktree {branch}"));
        context
            .repo
            .add_worktree(&worktree_path, &branch, base.as_deref())
            .map_err(|err| {
                events.step_failed(
                    "worktree",
                    format!("creating worktree {branch}"),
                    err.to_string(),
                );
                ApiError::from(err)
            })?;
        events.step_done("worktree", format!("creating worktree {branch}"));

        // Before anything starts: what git does not carry is exactly what
        // the services will fail without.
        crate::carry::files(
            &context.config.project.carry,
            &context.repo.main_root,
            &worktree_path,
            true,
            events,
        );

        // Register the new worktree, which settles the label its URLs
        // use.
        let record = {
            let _guard = self.state_lock.lock().await;
            let worktrees = context.repo.worktrees().map_err(ApiError::from)?;
            let created = worktrees
                .iter()
                .find(|wt| wt.path == worktree_path || wt.branch.as_deref() == Some(&branch))
                .cloned()
                .ok_or_else(|| {
                    ApiError::internal("git does not recognise the worktree that was just created")
                })?;

            self.store
                .update(|state| context.register(&created, state))
                .map_err(ApiError::from)?
        };

        let resolved = Resolved {
            repo: context.repo,
            config: context.config,
            project: context.project,
            workspace: record,
        };

        if start {
            self.start_services(&resolved, &[], rebuild, events).await?;
        }

        let statuses = self
            .refresh_for_display(&resolved.project, &resolved.config)
            .await?;

        Ok(Response::Workspace {
            workspace: self.build_workspace_info(
                &resolved.config,
                &resolved.project,
                &resolved.workspace,
                &statuses,
            ),
        })
    }

    async fn rm(
        &self,
        target: Target,
        force: bool,
        events: &EventSink,
    ) -> Result<Response, ApiError> {
        let resolved = self.resolve(&target).await?;

        if resolved.workspace.is_main {
            return Err(ApiError::new(
                ErrorCode::InvalidConfig,
                "the main worktree cannot be removed".to_string(),
            )
            .with_hint("name the workspace to remove with --workspace"));
        }

        let runtime = self.runtime(&resolved.config.runtime.default).await?;
        let key = WorkspaceKey::new(&resolved.project, &resolved.workspace.label);

        runtime.destroy_workspace(&key, events).await?;

        events.step_started("worktree", "removing the worktree");
        resolved
            .repo
            .remove_worktree(&resolved.workspace.path, force)
            .map_err(|err| {
                events.step_failed("worktree", "removing the worktree", err.to_string());
                ApiError::from(err)
                    .with_hint("pass --force when uncommitted changes are in the way")
            })?;
        events.step_done("worktree", "removing the worktree");

        {
            let _guard = self.state_lock.lock().await;
            self.store
                .update(|state| {
                    if let Some(project) = state.project_mut(&resolved.project) {
                        project.remove_workspace(&resolved.workspace.label);
                    }
                    Ok(())
                })
                .map_err(ApiError::from)?;
        }

        Ok(Response::Empty)
    }

    /// Takes down everything this daemon has made, across every project.
    ///
    /// The daemon's half of `minato uninstall`. It works from the state
    /// file rather than from a working directory, because by the time
    /// anyone uninstalls, the repository a project was registered from may
    /// well have been deleted already.
    ///
    /// **Worktrees are left exactly where they are.** They are the user's
    /// checkouts, with the user's uncommitted work in them. They are
    /// listed so the CLI can say what is being left behind.
    async fn purge(&self, dry_run: bool, events: &EventSink) -> Result<Response, ApiError> {
        let mut report = PurgeReport {
            dry_run,
            ..PurgeReport::default()
        };

        for project in self.known_projects().await? {
            // Everything that can fail per project is gathered here, so
            // one unreachable runtime cannot abort the others. A Docker
            // that is not running usually fails at `list_project` rather
            // than at resolving the runtime, so catching only the latter
            // would have caught almost nothing.
            let found = async {
                let runtime = self.purge_runtime(&project).await?;
                let statuses = runtime.list_project(&project).await?;
                let records = self.workspace_records(&project).await?;
                Ok::<_, ApiError>((runtime, statuses, records))
            }
            .await;

            let (runtime, statuses, records) = match found {
                Ok(found) => found,
                Err(err) => {
                    events.warn(format!("cannot reach {project}: {err}"));
                    report.stranded.push(minato_api::PurgeFailure {
                        project,
                        reason: err.to_string(),
                    });
                    continue;
                }
            };

            // Every workspace the runtime knows of, and every one the
            // state file does. A workspace can be in one and not the
            // other: a container started before a crash, or a record whose
            // containers are already gone.
            let mut workspaces: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for record in records {
                workspaces.entry(record.label).or_default();
                report.worktrees.push(record.path);
            }
            for status in &statuses {
                workspaces
                    .entry(status.key.workspace.workspace.clone())
                    .or_default()
                    .push(status.key.service.clone());
            }

            let mut left_behind: Option<String> = None;

            if !dry_run {
                for label in workspaces.keys() {
                    let key = WorkspaceKey::new(&project, label);
                    if let Err(err) = runtime.destroy_workspace(&key, events).await {
                        // Reported and carried past for the same reason:
                        // stopping here would leave the rest running with
                        // nothing left to manage them.
                        events.warn(format!("cannot remove {project}/{label}: {err}"));
                        left_behind.get_or_insert_with(|| err.to_string());
                    }
                }
            }

            if let Some(reason) = left_behind {
                report.stranded.push(minato_api::PurgeFailure {
                    project: project.clone(),
                    reason,
                });
            }

            report.projects.push(PurgeProject {
                name: project,
                workspaces: workspaces
                    .into_iter()
                    .map(|(label, mut services)| {
                        services.sort();
                        PurgeWorkspace { label, services }
                    })
                    .collect(),
            });
        }

        // The tunnel is machine-wide rather than per project, so it is
        // dealt with once, here.
        report.tunnel = self.purge_tunnel(dry_run, events).await;

        if !dry_run {
            events.step_started("state", "forgetting the projects that are gone");

            // Anything still standing keeps its entry, so a later run can
            // finish the job. Clearing the lot would forget the name of
            // every container that is still up, and there would be nothing
            // left that knew how to find them.
            let stranded: std::collections::BTreeSet<&str> = report
                .stranded
                .iter()
                .map(|failure| failure.project.as_str())
                .collect();

            let _guard = self.state_lock.lock().await;
            self.store
                .update(|state| {
                    state
                        .projects
                        .retain(|name, _| stranded.contains(name.as_str()));
                    Ok(())
                })
                .map_err(ApiError::from)?;

            events.step_done("state", "forgetting the projects that are gone");
        }

        report.worktrees.sort();
        report.worktrees.dedup();

        Ok(Response::Purge(report))
    }

    /// Stops the tunnel and says what is left in the Cloudflare account.
    ///
    /// The local half — the `cloudflared` process and the record in the
    /// state file — is Minato's to clean up and it does. The named tunnel
    /// and its DNS records are in the user's account, and an uninstaller
    /// that reached in there uninvited would be doing something no other
    /// command in this project does. So they are reported instead, with
    /// the command that removes them.
    async fn purge_tunnel(
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

    /// The runtime to tear a project down with.
    ///
    /// Its own `[runtime] default` when the configuration can still be
    /// read, and the built-in default when it cannot — a project whose
    /// repository has been deleted still has containers to remove, and
    /// giving up on them is how a machine ends up with orphans nothing
    /// knows the name of.
    async fn purge_runtime(&self, project: &str) -> Result<Arc<dyn Runtime>, ApiError> {
        let id = match self.project_config(project).await {
            Ok(config) => config.runtime.default,
            Err(_) => minato_core::RuntimeSection::default().default,
        };

        self.runtime(&id).await
    }

    async fn up(
        &self,
        target: Target,
        services: Vec<String>,
        rebuild: bool,
        events: &EventSink,
    ) -> Result<Response, ApiError> {
        let resolved = self.resolve(&target).await?;
        self.start_services(&resolved, &services, rebuild, events)
            .await?;

        let statuses = self
            .refresh_for_display(&resolved.project, &resolved.config)
            .await?;

        Ok(Response::Workspace {
            workspace: self.build_workspace_info(
                &resolved.config,
                &resolved.project,
                &resolved.workspace,
                &statuses,
            ),
        })
    }

    async fn start_services(
        &self,
        resolved: &Resolved,
        only: &[String],
        rebuild: bool,
        events: &EventSink,
    ) -> Result<(), ApiError> {
        let runtime = self.runtime(&resolved.config.runtime.default).await?;

        // Again on every start, not only when the worktree is made. Adding
        // `carry` to a project whose worktrees already exist would
        // otherwise do nothing at all, and the failure that follows is the
        // exact one the setting exists to prevent. Copying is a no-op once
        // the file is there, so this costs a stat per entry.
        if !resolved.workspace.is_main {
            crate::carry::files(
                &resolved.config.project.carry,
                &resolved.repo.main_root,
                &resolved.workspace.path,
                false,
                events,
            );
        }

        // **Said out loud, before anything starts.** With no proxy there is
        // no URL to hand out, so `MINATO_URL_<SERVICE>` is left unset — and
        // inside the container that surfaces as `parameter not set` from a
        // start-up script, which names nothing that leads back to here.
        if !self.gateway.is_serving() {
            events.warn(
                "the proxy is not listening, so no MINATO_URL_<SERVICE> is \
                 injected and the URLs will not answer. `minato doctor` says \
                 what to do about it"
                    .to_string(),
            );
        }

        let envs = self
            .workspace_envs(
                &resolved.config,
                &resolved.project,
                &resolved.workspace,
                &resolved.repo.main_root,
                events,
            )
            .await?;

        let workspace_spec = spec::build_workspace_spec(
            &resolved.config,
            &resolved.project,
            &resolved.workspace.label,
            &resolved.workspace.path,
            &envs,
        )?;

        // Even a narrowed selection has to bring its dependencies up.
        let selected = select_with_dependencies(&resolved.config, only)?;

        let filtered: Vec<_> = workspace_spec
            .services
            .iter()
            .filter(|s| selected.contains(&s.name().to_string()))
            .cloned()
            .collect();

        if filtered.is_empty() {
            return Err(ApiError::new(
                ErrorCode::NotFound,
                "there is nothing to start".to_string(),
            ));
        }

        let prepare_spec = minato_runtime::WorkspaceSpec {
            key: workspace_spec.key.clone(),
            worktree_path: workspace_spec.worktree_path.clone(),
            services: filtered.clone(),
        };

        runtime.prepare(&prepare_spec, rebuild, events).await?;

        // Started in startup_order, each one set up just before it starts.
        //
        // **Interleaved, not done in a batch first.** A setup that needs a
        // dependency — migrations against `db` — has to run after the
        // thing it depends on is up, and `startup_order` already puts them
        // in that order.
        for service in &filtered {
            self.run_setup(resolved, service, runtime.as_ref(), events)
                .await?;
            runtime.start(service, events).await?;
        }

        Ok(())
    }

    /// Runs a service's `setup`, if it has not had this one.
    ///
    /// **Before the service starts**, and in a throwaway container, so the
    /// start-up command is left doing nothing but starting the app — which
    /// was the point of asking for this. The throwaway carries the
    /// service's image, environment and volumes, so what it installs is
    /// there when the real container comes up.
    ///
    /// Remembered against the worktree rather than the container: a stopped
    /// container is recreated by the next `up`, so anything keyed on
    /// container creation would run on every `down`/`up`.
    async fn run_setup(
        &self,
        resolved: &Resolved,
        spec: &minato_runtime::ServiceSpec,
        runtime: &dyn Runtime,
        events: &EventSink,
    ) -> Result<(), ApiError> {
        let name = spec.name();
        let service = resolved.config.service(name).map_err(ApiError::from)?;
        let Some(setup) = service.setup.clone() else {
            return Ok(());
        };

        let command = shell_words::split(&setup).map_err(|err| {
            ApiError::new(
                ErrorCode::InvalidConfig,
                format!("service `{name}`: cannot make sense of setup: {err}"),
            )
        })?;

        if command.is_empty() {
            return Err(ApiError::new(
                ErrorCode::InvalidConfig,
                format!("service `{name}`: setup is empty"),
            ));
        }

        // **Held across the check and the record, not just the write.**
        // Two `up`s racing would otherwise both decide it was needed and
        // both run an install into the same volume, then both remember the
        // result as good.
        let _guard = self.setup_lock.lock().await;

        let project = resolved.project.clone();
        let workspace = resolved.workspace.label.clone();

        let pending = self.store.load().map_err(ApiError::from)?.needs_setup(
            &project,
            &workspace,
            name,
            service.scope,
            &setup,
        );

        if !pending {
            return Ok(());
        }

        let step = format!("setup-{name}");
        let label = format!("setting {name} up");
        events.step_started(&step, &label);

        let outcome = match runtime
            .exec_fresh(spec, &command, &Default::default(), events)
            .await
        {
            Ok(outcome) => outcome,
            Err(err) => {
                events.step_failed(&step, &label, err.to_string());
                return Err(err.into());
            }
        };

        if outcome.exit_code != 0 {
            events.step_failed(&step, &label, format!("exited with {}", outcome.exit_code));
            return Err(ApiError::new(
                ErrorCode::RuntimeFailed,
                format!("service `{name}`: setup exited with {}", outcome.exit_code),
            )
            .with_hint("the output above says what happened. Fix it and run `minato up` again"));
        }

        events.step_done(&step, &label);

        // Recorded only once it has worked. A setup that failed has not
        // run, whatever it managed to do before giving up.
        let service_name = name.to_string();
        let scope = service.scope;
        let recorded = self
            .store
            .update(|state| {
                Ok(state.record_setup(&project, &workspace, &service_name, scope, &setup))
            })
            .map_err(ApiError::from)?;

        if !recorded {
            events.debug(format!(
                "{name} was set up, but the workspace went before it could be remembered"
            ));
        }

        Ok(())
    }

    async fn down(
        &self,
        target: Target,
        services: Vec<String>,
        all: bool,
        events: &EventSink,
    ) -> Result<Response, ApiError> {
        let resolved = self.resolve(&target).await?;
        let runtime = self.runtime(&resolved.config.runtime.default).await?;

        if all {
            // Stop every Minato-managed service in the project.
            let statuses = runtime.list_project(&resolved.project).await?;
            for status in statuses {
                if status.state.is_running() {
                    runtime.stop(&status.key, events).await?;
                }
            }
        } else {
            let key = WorkspaceKey::new(&resolved.project, &resolved.workspace.label);

            // Naming services explicitly changes what happens to the
            // shared ones.
            let explicit = !services.is_empty();
            let targets: Vec<String> = if explicit {
                validate_service_names(&resolved.config, &services)?;
                services
            } else {
                resolved.config.services.keys().cloned().collect()
            };

            for name in targets {
                let service_config = resolved.config.service(&name).map_err(ApiError::from)?;

                // Other workspaces use a shared service too, so it only
                // stops when it was named.
                if service_config.scope == ServiceScope::Project && !explicit {
                    events.step_skipped(
                        "stop",
                        format!("stopping {name}"),
                        "a shared service only stops when it is named",
                    );
                    continue;
                }

                let service_key = match service_config.scope {
                    ServiceScope::Workspace => key.service(&name),
                    ServiceScope::Project => WorkspaceKey::shared(&resolved.project).service(&name),
                };

                runtime.stop(&service_key, events).await?;
            }
        }

        let statuses = self
            .refresh_for_display(&resolved.project, &resolved.config)
            .await?;

        Ok(Response::Workspace {
            workspace: self.build_workspace_info(
                &resolved.config,
                &resolved.project,
                &resolved.workspace,
                &statuses,
            ),
        })
    }
}

/// How long a single readiness glance may take.
///
/// Not [`minato_runtime::DEFAULT_READINESS_TIMEOUT`], which is how long
/// *starting* waits for an app to come up. This is a question asked while
/// someone waits for the answer, and a check that has not replied over
/// loopback by now is not serving. Reporting `starting` after a second
/// beats making `minato status` sit there.
const READINESS_GLANCE: Duration = Duration::from_secs(1);

/// Narrows `ready` to `starting` for a container whose app is not answering.
///
/// **A container being up and the app inside being able to answer are two
/// different things.** Docker reports `running` the moment the process
/// exists, so a dev server that compiles for a minute, or a start-up script
/// blocked on a lock, looks exactly like one serving requests.
///
/// **Only an HTTP `health` can settle that from out here**, and that is the
/// only case this touches. A connection attempt cannot: Docker publishes a
/// port by putting a forwarder in front of it, and that forwarder accepts
/// immediately whether or not anything inside is listening — measured, not
/// assumed. Probing TCP would hand back `ready` for a container with
/// nothing running in it, which is worse than not asking, and would spend a
/// connection per service per listing to do it.
///
/// Only ever downgrades, so this makes the state more accurate and never
/// less.
async fn settle_readiness(config: &MinatoConfig, statuses: &mut [ServiceStatus]) {
    let pending: Vec<_> = statuses
        .iter()
        .enumerate()
        .filter(|(_, status)| status.state == ServiceState::Ready)
        .filter_map(|(index, status)| {
            let endpoint = status.endpoint?;

            // `tcp://` is the same connection attempt under another name,
            // and `cmd:` would need an exec per service per listing.
            let health = match config.service(&status.key.service).ok()?.health.clone()? {
                health @ HealthCheck::Http(_) => health,
                HealthCheck::Tcp(_) | HealthCheck::Cmd(_) => return None,
            };

            Some(async move {
                let answered = tokio::time::timeout(
                    READINESS_GLANCE,
                    minato_runtime::probe(endpoint, Some(&health), None),
                )
                .await;

                // Running out of time counts as not answering: the check
                // was asked over loopback and did not reply, which is the
                // shape of an app that has bound its port and is still
                // compiling.
                (index, !matches!(answered, Ok(Ok(true))))
            })
        })
        .collect();

    if pending.is_empty() {
        return;
    }

    for (index, still_starting) in futures::future::join_all(pending).await {
        if still_starting {
            statuses[index].state = ServiceState::Starting;
        }
    }
}

/// An address as it is written down: `[::1]`, not `::1`.
///
/// `Display` for an `IpAddr` gives the bare form, which reads as a stray
/// colon run in a sentence and does not match how the docs or the URLs
/// write it.
fn bracketed(address: std::net::IpAddr) -> String {
    match address {
        std::net::IpAddr::V4(address) => address.to_string(),
        std::net::IpAddr::V6(address) => format!("[{address}]"),
    }
}

/// How a listener that did come up is described.
///
/// **Says when it had to settle.** Landing on the fallback is not a failure
/// — URLs work — but they carry a port from then on, and without a word
/// here that reads as an oddity rather than the consequence of a privilege
/// the daemon never had.
///
/// Keyed on having fallen back rather than on the port not being 80: a port
/// named with `MINATO_HTTP_PORT` is what was asked for, and calling that
/// unexpected would be wrong.
fn listening_detail(port: u16, fell_back: bool) -> String {
    if !fell_back {
        return format!("127.0.0.1:{port}");
    }

    format!("127.0.0.1:{port} (a fallback, so every URL carries the port)")
}

/// How a missing listener is described.
fn detail_for(failure: Option<BindFailure>) -> String {
    failure.unwrap_or(BindFailure::Other).detail().to_string()
}

/// What to do about a listener that could not be held.
///
/// **The launchd case comes first.** After `minato daemon stop` the job is
/// idle while launchd keeps holding 80, so the bind fails with the port in
/// use — and the old advice, "a port below 1024 needs privileges, follow
/// `minato setup`", names neither the cause nor a step that helps.
///
/// `launchd_installed` is passed in rather than read here, so the advice can
/// be checked without a plist on the machine running the tests.
fn bind_fix(failure: Option<BindFailure>, port_env: &str, launchd_installed: bool) -> String {
    if failure == Some(BindFailure::InUse) && launchd_installed {
        return format!(
            "launchd may be holding this port for a job it is not running. \
             `minato daemon start` wakes it; failing that, run `{}`. If \
             something unrelated has the port, name another with {port_env}",
            minato_core::launchd::kickstart_command()
        );
    }

    match failure.unwrap_or(BindFailure::Other) {
        BindFailure::Privileged => format!(
            "a port below 1024 needs privileges. Follow `minato setup`, \
             or name another port with {port_env}"
        ),
        BindFailure::InUse => {
            format!("stop whatever else holds the port, or name another with {port_env}")
        }
        BindFailure::Other => format!("name another port with {port_env}"),
    }
}

/// The named services, plus everything they depend on.
fn select_with_dependencies(
    config: &MinatoConfig,
    only: &[String],
) -> Result<Vec<String>, ApiError> {
    if only.is_empty() {
        return Ok(config.services.keys().cloned().collect());
    }

    validate_service_names(config, only)?;

    let mut selected: Vec<String> = Vec::new();
    let mut stack: Vec<String> = only.to_vec();

    while let Some(name) = stack.pop() {
        if selected.contains(&name) {
            continue;
        }

        if let Some(service) = config.services.get(&name) {
            for dep in &service.depends_on {
                if !selected.contains(dep) {
                    stack.push(dep.clone());
                }
            }
        }

        selected.push(name);
    }

    Ok(selected)
}

fn validate_service_names(config: &MinatoConfig, names: &[String]) -> Result<(), ApiError> {
    for name in names {
        if !config.services.contains_key(name) {
            let available: Vec<&str> = config.services.keys().map(String::as_str).collect();
            return Err(ApiError::not_found(format!("no service named `{name}`"))
                .with_hint(format!("available: {}", available.join(", "))));
        }
    }
    Ok(())
}

/// Where a worktree goes by default.
///
/// `{repository}.wt/{branch}`, alongside the repository. Inside it, the
/// worktree would end up in editors and searches.
fn default_worktree_path(main_root: &std::path::Path, branch: &str) -> PathBuf {
    let repo_name = main_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".to_string());

    let parent = main_root.parent().unwrap_or(main_root);
    let label = minato_core::naming::sanitize_label(branch);

    parent.join(format!("{repo_name}.wt")).join(label)
}

/// What `minato new` was asked for, beyond the target.
///
/// A struct rather than five more parameters: they arrive together, from
/// one request, and travel as a unit.
struct NewWorkspace {
    branch: String,
    base: Option<String>,
    path: Option<PathBuf>,
    /// Whether to start the services once the worktree exists.
    start: bool,
    /// Whether to rebuild images that are already built.
    rebuild: bool,
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

/// Says the value was written, whatever the listing that follows does.
///
/// **`env set` and `env unset` answer with a listing, and settling the
/// layers can fail** — a `${...}` somewhere refers to a name nothing sets.
/// The value is on disk by then, so an error that only described the
/// listing would read as though nothing had been written, and invite the
/// same command again.
fn written(key: String, listing: Result<Response, ApiError>) -> Result<Response, ApiError> {
    listing.map_err(|err| ApiError {
        message: format!("{key} was written. {}", err.message),
        ..err
    })
}

/// Reads a file, or an empty string when there is none.
fn read_or_empty(path: &std::path::Path) -> Result<String, ApiError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(ApiError::internal(format!(
            "cannot read {}: {err}",
            path.display()
        ))),
    }
}

/// Builds the hostname-to-target list the proxy registers.
///
/// **Stopped services are listed too.** Scale-to-zero has to tell
/// "stopped" apart from "does not exist": the first is woken by a request,
/// the second gets a 404. Only running services get a target.
fn route_entries(
    config: &MinatoConfig,
    project: &str,
    records: &[WorkspaceRecord],
    statuses: &[ServiceStatus],
    tunnel_domain: Option<&str>,
) -> Vec<(String, Route)> {
    let domain = config.domain();
    let shared_key = WorkspaceKey::shared(project);
    let mut entries = Vec::new();

    for record in records {
        let workspace_key = WorkspaceKey::new(project, &record.label);

        for (name, service_config) in &config.services {
            if !service_config.exposed() {
                continue;
            }

            let key = match service_config.scope {
                ServiceScope::Workspace => workspace_key.service(name),
                ServiceScope::Project => shared_key.service(name),
            };

            let status = statuses.iter().find(|s| s.key == key);
            let endpoint = status
                .filter(|status| status.state.is_running())
                .and_then(|status| status.endpoint);

            let host = minato_core::naming::service_host_in(name, record.url_label(), &domain);
            let route = match endpoint {
                Some(endpoint) => Route::new(endpoint, project, &record.label, name.clone()),
                None => Route::stopped(project, &record.label, name.clone()),
            };

            // The tunnel hostname resolves to the same service.
            //
            // This is what lets cloudflared run one wildcard ingress rule
            // that never changes: the Host header arrives untouched and
            // the proxy already knows what it means. Rewriting Host at the
            // tunnel instead would need a rule per service, regenerated
            // and reloaded every time a worktree appeared.
            if let Some(tunnel_domain) = tunnel_domain {
                let tunnel = minato_core::naming::tunnel_host(
                    name,
                    record.url_label(),
                    project,
                    tunnel_domain,
                );
                entries.push((tunnel, route.clone()));
            }

            entries.push((host, route));
        }
    }

    entries
}

impl Supervisor {
    /// Puts the configuration and the runtime's state together into what
    /// a client receives.
    fn build_workspace_info(
        &self,
        config: &MinatoConfig,
        project: &str,
        record: &WorkspaceRecord,
        statuses: &[ServiceStatus],
    ) -> WorkspaceInfo {
        let workspace_key = WorkspaceKey::new(project, &record.label);
        let shared_key = WorkspaceKey::shared(project);
        let domain = config.domain();
        let tunnel_domain = self.tunnel.domain();

        let services = config
            .services
            .iter()
            .map(|(name, service_config)| {
                let key = match service_config.scope {
                    ServiceScope::Workspace => workspace_key.service(name),
                    ServiceScope::Project => shared_key.service(name),
                };

                let status = statuses.iter().find(|s| s.key == key);
                let state = status
                    .map(|s| s.state.clone())
                    .unwrap_or(ServiceState::Stopped);

                // A stopped service still gets its URL: reaching for it
                // starts it, so the URL is the right thing to point at. A
                // URL that came and went with the state would read as
                // "the URL that worked a minute ago is gone".
                let url = if service_config.exposed() {
                    let host =
                        minato_core::naming::service_host_in(name, record.url_label(), &domain);
                    self.gateway.url_for(&host)
                } else {
                    None
                };

                // Only while the tunnel is actually up. A URL for a
                // tunnel that is down points at a 502, and unlike the
                // local URL there is nothing a request can do to wake it.
                let tunnel_url = tunnel_domain
                    .as_deref()
                    .filter(|_| service_config.exposed())
                    .map(|tunnel_domain| {
                        let host = minato_core::naming::tunnel_host(
                            name,
                            record.url_label(),
                            project,
                            tunnel_domain,
                        );
                        format!("https://{host}")
                    });

                ServiceInfo {
                    name: name.clone(),
                    state,
                    scope: service_config.scope,
                    url,
                    tunnel_url,
                    endpoint: status
                        .and_then(|s| s.endpoint)
                        .filter(|_| service_config.exposed())
                        .map(|addr| addr.to_string()),
                    port: service_config.port,
                    container_id: status.and_then(|s| s.container_id.clone()),
                    image: status
                        .and_then(|s| s.image.clone())
                        .or_else(|| service_config.image.clone()),
                }
            })
            .collect();

        WorkspaceInfo {
            project: project.to_string(),
            workspace: record.url_label().map(str::to_string),
            branch: record.branch.clone(),
            path: record.path.clone(),
            is_main: record.is_main,
            services,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::Gateway;
    use std::net::SocketAddr;
    use std::path::Path;

    /// A supervisor for looking at URL building and routing alone. It
    /// never touches the state store, so a path that does not exist is
    /// fine.
    fn supervisor(gateway: Gateway) -> Supervisor {
        Supervisor::new(
            &Paths::with_root(PathBuf::from("/tmp/minato-supervisor-test")),
            Arc::new(gateway),
            TunnelHandle::new(),
            Arc::new(tokio::sync::Notify::new()),
        )
    }

    fn ready(key: minato_runtime::ServiceKey, port: u16, scope: ServiceScope) -> ServiceStatus {
        ServiceStatus {
            key,
            state: ServiceState::Ready,
            container_id: Some("abc".into()),
            image: Some("busybox".into()),
            endpoint: Some(SocketAddr::from(([127, 0, 0, 1], port))),
            port: Some(port),
            scope,
        }
    }

    fn config(toml: &str) -> MinatoConfig {
        let config: MinatoConfig = toml::from_str(toml).expect("is syntactically valid");
        config.validate().expect("is semantically valid");
        config
    }

    const SAMPLE: &str = r#"
        [project]
        name = "myapp"
        [services.web]
        image = "node:22"
        port = 3000
        depends_on = ["api"]
        [services.api]
        image = "node:22"
        port = 8080
        depends_on = ["db"]
        [services.db]
        image = "postgres:16"
        port = 5432
        scope = "project"
        expose = false
    "#;

    #[test]
    fn a_port_in_use_under_launchd_points_at_launchd() {
        // The state `minato daemon stop` leaves behind: the job is idle,
        // launchd still holds 80. Advising `minato setup` here sends
        // someone to re-run what they have already done.
        let fix = bind_fix(Some(BindFailure::InUse), "MINATO_HTTP_PORT", true);

        assert!(fix.contains("launchd"), "name the cause: {fix}");
        assert!(fix.contains("minato daemon start"), "{fix}");
        assert!(
            !fix.contains("minato setup"),
            "setup is already done in this state: {fix}"
        );
    }

    #[test]
    fn a_privileged_port_still_points_at_setup() {
        let fix = bind_fix(Some(BindFailure::Privileged), "MINATO_HTTP_PORT", false);

        assert!(fix.contains("minato setup"), "{fix}");
        assert!(fix.contains("MINATO_HTTP_PORT"), "{fix}");
    }

    #[test]
    fn a_port_in_use_without_launchd_blames_the_other_process() {
        let fix = bind_fix(Some(BindFailure::InUse), "MINATO_DNS_PORT", false);

        assert!(!fix.contains("launchd"), "{fix}");
        assert!(fix.contains("MINATO_DNS_PORT"), "{fix}");
    }

    #[test]
    fn the_detail_says_which_kind_of_failure_it_was() {
        // "could not be held" was all it ever said, whatever happened.
        assert!(detail_for(Some(BindFailure::Privileged)).contains("privileges"));
        assert!(detail_for(Some(BindFailure::InUse)).contains("another process"));
    }

    #[test]
    fn an_address_reads_the_way_it_is_written_down() {
        // `Display` gives `::1`, which reads as a stray colon run in a
        // sentence and matches neither the docs nor a URL.
        assert_eq!(
            bracketed(std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)),
            "[::1]"
        );
        assert_eq!(
            bracketed(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
            "127.0.0.1"
        );
    }

    #[test]
    fn a_fallback_port_is_reported_as_such() {
        // Landing on the fallback is not a failure, but the URLs carry a
        // port from then on. Left unsaid that reads as an oddity rather
        // than the consequence of a privilege the daemon never had.
        let detail = listening_detail(crate::gateway::FALLBACK_HTTPS_PORT, true);

        assert!(detail.contains("18443"), "{detail}");
        assert!(detail.contains("fallback"), "{detail}");
        assert!(detail.contains("carries the port"), "{detail}");
    }

    #[test]
    fn a_port_that_was_asked_for_is_reported_plainly() {
        // MINATO_HTTPS_PORT=8443 got exactly what it named. Calling that a
        // fallback would present the user's own choice as an anomaly.
        assert_eq!(listening_detail(8443, false), "127.0.0.1:8443");
        assert_eq!(listening_detail(443, false), "127.0.0.1:443");
    }

    /// A config whose only service is `web`, so `ready(...)` lines up.
    ///
    /// `health` is what makes readiness answerable from outside the
    /// container, so it is the shape worth testing against.
    fn web_with_http_health() -> MinatoConfig {
        config(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            port = 3000
            health = "http://localhost:3000/healthz"
        "#,
        )
    }

    /// The same, with nothing declaring how readiness is decided.
    fn web_only() -> MinatoConfig {
        config(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            port = 3000
        "#,
        )
    }

    /// A port that was bound and released: connections are refused rather
    /// than left hanging.
    async fn closed_port() -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("binds");
        let port = listener.local_addr().expect("bound").port();
        drop(listener);
        port
    }

    #[tokio::test]
    async fn a_health_check_that_does_not_answer_means_starting() {
        // Docker says `running` as soon as the process exists. A dev server
        // compiling for a minute looked exactly like one serving requests,
        // which is the question `minato status` is for.
        let key = WorkspaceKey::new("myapp", "feat-1").service("web");
        let mut statuses = vec![ready(key, closed_port().await, ServiceScope::Workspace)];

        settle_readiness(&web_with_http_health(), &mut statuses).await;

        assert_eq!(statuses[0].state, ServiceState::Starting);
    }

    #[tokio::test]
    async fn a_health_check_that_answers_stays_ready() {
        let key = WorkspaceKey::new("myapp", "feat-1").service("web");

        // Answers one request with a 200 and goes away.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("binds");
        let port = listener.local_addr().expect("bound").port();

        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                use tokio::io::AsyncWriteExt;
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                    .await;
            }
        });

        let mut statuses = vec![ready(key, port, ServiceScope::Workspace)];
        settle_readiness(&web_with_http_health(), &mut statuses).await;

        assert_eq!(statuses[0].state, ServiceState::Ready);
    }

    #[tokio::test]
    async fn without_a_health_check_the_runtime_answer_stands() {
        // **Measured, not assumed.** Docker publishes a port by putting a
        // forwarder in front of it, and that forwarder accepts the moment
        // the container starts, whether or not anything inside is
        // listening. A connection attempt would hand back `ready` for a
        // container running nothing at all.
        let key = WorkspaceKey::new("myapp", "feat-1").service("web");
        let mut statuses = vec![ready(key, closed_port().await, ServiceScope::Workspace)];

        settle_readiness(&web_only(), &mut statuses).await;

        assert_eq!(statuses[0].state, ServiceState::Ready);
    }

    #[tokio::test]
    async fn a_tcp_health_check_is_not_probed_either() {
        // Same connection attempt under another name, so it tells us the
        // same nothing.
        let config = config(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            port = 3000
            health = "tcp://localhost:3000"
        "#,
        );

        let key = WorkspaceKey::new("myapp", "feat-1").service("web");
        let mut statuses = vec![ready(key, closed_port().await, ServiceScope::Workspace)];

        settle_readiness(&config, &mut statuses).await;

        assert_eq!(statuses[0].state, ServiceState::Ready);
    }

    #[tokio::test]
    async fn a_service_with_no_endpoint_is_left_alone() {
        // Nothing to connect to, so there is nothing to learn. Guessing
        // `starting` would make every unexposed service look stuck.
        let key = WorkspaceKey::new("myapp", "feat-1").service("web");
        let mut statuses = vec![ServiceStatus {
            endpoint: None,
            ..ready(key, 3000, ServiceScope::Workspace)
        }];

        settle_readiness(&web_with_http_health(), &mut statuses).await;

        assert_eq!(statuses[0].state, ServiceState::Ready);
    }

    #[tokio::test]
    async fn only_ready_is_ever_narrowed() {
        // Downgrading only. A stopped or failed service must not be talked
        // into looking like it is on its way up.
        let key = WorkspaceKey::new("myapp", "feat-1").service("web");

        let port = closed_port().await;

        for state in [
            ServiceState::Stopped,
            ServiceState::failed("it fell over"),
            ServiceState::Unknown,
        ] {
            let mut statuses = vec![ServiceStatus {
                state: state.clone(),
                ..ready(key.clone(), port, ServiceScope::Workspace)
            }];

            settle_readiness(&web_with_http_health(), &mut statuses).await;
            assert_eq!(statuses[0].state, state);
        }
    }

    #[tokio::test]
    async fn a_cmd_health_check_keeps_the_runtime_answer() {
        // Running one would cost an exec per service per listing, so it
        // cannot be evaluated here — and an unanswerable question is not
        // grounds for saying the service is not up.
        let config = config(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            port = 3000
            health = "cmd:true"
        "#,
        );

        let key = WorkspaceKey::new("myapp", "feat-1").service("web");
        let mut statuses = vec![ready(key, closed_port().await, ServiceScope::Workspace)];

        settle_readiness(&config, &mut statuses).await;

        assert_eq!(statuses[0].state, ServiceState::Ready);
    }

    #[test]
    fn selecting_a_service_pulls_in_its_dependencies() {
        let config = config(SAMPLE);
        let selected = select_with_dependencies(&config, &["web".to_string()]).expect("resolves");

        assert!(selected.contains(&"web".to_string()));
        assert!(
            selected.contains(&"api".to_string()),
            "transitive dependencies too"
        );
        assert!(
            selected.contains(&"db".to_string()),
            "transitive dependencies too"
        );
    }

    #[test]
    fn empty_selection_means_everything() {
        let config = config(SAMPLE);
        let selected = select_with_dependencies(&config, &[]).expect("resolves");
        assert_eq!(selected.len(), 3);
    }

    #[test]
    fn unknown_service_lists_the_available_ones() {
        let config = config(SAMPLE);
        let err = select_with_dependencies(&config, &["nope".to_string()]).unwrap_err();

        assert_eq!(err.code, ErrorCode::NotFound);
        let hint = err.hint.expect("has a hint");
        assert!(hint.contains("web") && hint.contains("api"), "got: {hint}");
    }

    #[test]
    fn worktree_path_sits_beside_the_repository() {
        let path =
            default_worktree_path(Path::new("/Users/x/ghq/github.com/y/myapp"), "feature/one");

        assert_eq!(
            path,
            PathBuf::from("/Users/x/ghq/github.com/y/myapp.wt/feature-one"),
            "inside the repository it would end up in editor searches"
        );
    }

    fn record(label: &str, is_main: bool) -> minato_core::WorkspaceRecord {
        minato_core::WorkspaceRecord {
            label: label.to_string(),
            branch: "feature/one".to_string(),
            path: PathBuf::from("/repo/wt/feat-1"),
            is_main,
            created_at: chrono::Utc::now(),
            setup_done: Default::default(),
        }
    }

    #[test]
    fn reports_stopped_when_runtime_knows_nothing() {
        let info = supervisor(Gateway::with_ports(Some(80), Some(443))).build_workspace_info(
            &config(SAMPLE),
            "myapp",
            &record("feat-1", false),
            &[],
        );

        assert_eq!(info.services.len(), 3);
        for service in &info.services {
            assert_eq!(service.state, ServiceState::Stopped);
            assert_eq!(service.endpoint, None);
        }
    }

    #[test]
    fn matches_shared_services_against_the_shared_key() {
        let statuses = vec![ServiceStatus {
            key: WorkspaceKey::shared("myapp").service("db"),
            state: ServiceState::Ready,
            container_id: Some("abc".into()),
            image: Some("postgres:16".into()),
            endpoint: Some("127.0.0.1:5432".parse().expect("valid")),
            port: Some(5432),
            scope: ServiceScope::Project,
        }];

        let info = supervisor(Gateway::inert()).build_workspace_info(
            &config(SAMPLE),
            "myapp",
            &record("feat-1", false),
            &statuses,
        );
        let db = info.service("db").expect("exists");

        assert_eq!(
            db.state,
            ServiceState::Ready,
            "a shared service has a state too"
        );
        assert_eq!(db.endpoint, None, "expose = false, so no address is shown");
    }

    #[test]
    fn exposes_endpoint_for_published_services() {
        let statuses = vec![ServiceStatus {
            key: WorkspaceKey::new("myapp", "feat-1").service("web"),
            state: ServiceState::Ready,
            container_id: Some("abc".into()),
            image: Some("node:22".into()),
            endpoint: Some("127.0.0.1:49312".parse().expect("valid")),
            port: Some(3000),
            scope: ServiceScope::Workspace,
        }];

        let info = supervisor(Gateway::inert()).build_workspace_info(
            &config(SAMPLE),
            "myapp",
            &record("feat-1", false),
            &statuses,
        );
        let web = info.service("web").expect("exists");

        assert_eq!(web.endpoint.as_deref(), Some("127.0.0.1:49312"));
        assert_eq!(web.access().as_deref(), Some("http://127.0.0.1:49312"));
    }

    #[test]
    fn main_workspace_has_no_url_label() {
        let info = supervisor(Gateway::inert()).build_workspace_info(
            &config(SAMPLE),
            "myapp",
            &record("main", true),
            &[],
        );

        assert_eq!(info.workspace, None, "main is left out of the URL");
        assert!(info.is_main);
        assert_eq!(info.display_name(), "(main)");
    }

    #[test]
    fn issues_urls_when_the_proxy_is_listening() {
        let statuses = vec![ready(
            WorkspaceKey::new("myapp", "feat-1").service("web"),
            49312,
            ServiceScope::Workspace,
        )];

        let info = supervisor(Gateway::with_ports(Some(80), Some(443))).build_workspace_info(
            &config(SAMPLE),
            "myapp",
            &record("feat-1", false),
            &statuses,
        );

        let web = info.service("web").expect("exists");
        assert_eq!(
            web.url.as_deref(),
            Some("https://web.feat-1.myapp.localhost")
        );
        assert_eq!(
            web.access().as_deref(),
            Some("https://web.feat-1.myapp.localhost"),
            "with a URL, that is what gets pointed at"
        );
    }

    #[test]
    fn issues_no_url_when_the_proxy_is_down() {
        // A URL with nothing listening behind it points at a dead end.
        let statuses = vec![ready(
            WorkspaceKey::new("myapp", "feat-1").service("web"),
            49312,
            ServiceScope::Workspace,
        )];

        let info = supervisor(Gateway::inert()).build_workspace_info(
            &config(SAMPLE),
            "myapp",
            &record("feat-1", false),
            &statuses,
        );

        let web = info.service("web").expect("exists");
        assert_eq!(web.url, None);
        assert_eq!(
            web.access().as_deref(),
            Some("http://127.0.0.1:49312"),
            "without a URL there is still the raw address"
        );
    }

    #[test]
    fn main_workspace_url_omits_the_label() {
        let statuses = vec![ready(
            WorkspaceKey::new("myapp", "main").service("web"),
            49312,
            ServiceScope::Workspace,
        )];

        let info = supervisor(Gateway::with_ports(Some(80), Some(443))).build_workspace_info(
            &config(SAMPLE),
            "myapp",
            &record("main", true),
            &statuses,
        );

        assert_eq!(
            info.service("web").expect("exists").url.as_deref(),
            Some("https://web.myapp.localhost")
        );
    }

    #[test]
    fn stopped_services_still_advertise_their_url() {
        // Under scale-to-zero a request is what starts a service. A URL
        // that disappears takes the way to wake it with it.
        let info = supervisor(Gateway::with_ports(Some(80), Some(443))).build_workspace_info(
            &config(SAMPLE),
            "myapp",
            &record("feat-1", false),
            &[],
        );

        let web = info.service("web").expect("exists");
        assert_eq!(web.state, ServiceState::Stopped);
        assert_eq!(
            web.url.as_deref(),
            Some("https://web.feat-1.myapp.localhost")
        );

        // An unexposed service has no URL, stopped or not.
        assert_eq!(info.service("db").expect("exists").url, None);
    }

    #[test]
    fn routes_only_running_exposed_services() {
        let records = vec![record("feat-1", false)];
        let statuses = vec![
            ready(
                WorkspaceKey::new("myapp", "feat-1").service("web"),
                49312,
                ServiceScope::Workspace,
            ),
            // expose = false, so neither a URL nor a route.
            ready(
                WorkspaceKey::shared("myapp").service("db"),
                5432,
                ServiceScope::Project,
            ),
            // A stopped service is no target; it would only 502.
            ServiceStatus {
                key: WorkspaceKey::new("myapp", "feat-1").service("api"),
                state: ServiceState::Stopped,
                container_id: None,
                image: None,
                endpoint: None,
                port: Some(8080),
                scope: ServiceScope::Workspace,
            },
        ];

        let entries = route_entries(&config(SAMPLE), "myapp", &records, &statuses, None);

        let running: Vec<&str> = entries
            .iter()
            .filter(|(_, route)| route.is_running())
            .map(|(host, _)| host.as_str())
            .collect();
        assert_eq!(running, vec!["web.feat-1.myapp.localhost"]);

        // A stopped api is registered as existing, so a request can wake
        // it.
        let stopped: Vec<&str> = entries
            .iter()
            .filter(|(_, route)| !route.is_running())
            .map(|(host, _)| host.as_str())
            .collect();
        assert_eq!(stopped, vec!["api.feat-1.myapp.localhost"]);

        // db has expose = false and appears in neither.
        assert!(!entries.iter().any(|(host, _)| host.starts_with("db.")));

        let web = entries
            .iter()
            .find(|(host, _)| host.starts_with("web."))
            .expect("is there");
        assert_eq!(web.1.endpoint.expect("running").port(), 49312);
    }

    #[test]
    fn routes_every_workspace_of_the_project() {
        let records = vec![record("feat-1", false), record("main", true)];
        let statuses = vec![
            ready(
                WorkspaceKey::new("myapp", "feat-1").service("web"),
                49312,
                ServiceScope::Workspace,
            ),
            ready(
                WorkspaceKey::new("myapp", "main").service("web"),
                49313,
                ServiceScope::Workspace,
            ),
        ];

        let entries = route_entries(&config(SAMPLE), "myapp", &records, &statuses, None);
        let mut hosts: Vec<&str> = entries
            .iter()
            .filter(|(_, route)| route.is_running())
            .map(|(host, _)| host.as_str())
            .collect();
        hosts.sort();

        assert_eq!(
            hosts,
            vec!["web.feat-1.myapp.localhost", "web.myapp.localhost"]
        );
    }

    #[test]
    fn a_tunnel_hostname_reaches_the_same_service() {
        // This is what lets cloudflared run one wildcard ingress rule that
        // never changes. Without the second key, every request through the
        // tunnel arrives with a Host the proxy has never heard of and 404s.
        let records = vec![record("feat-1", false)];
        let statuses = vec![ready(
            WorkspaceKey::new("myapp", "feat-1").service("web"),
            49312,
            ServiceScope::Workspace,
        )];

        let entries = route_entries(
            &config(SAMPLE),
            "myapp",
            &records,
            &statuses,
            Some("example.com"),
        );

        let local = entries
            .iter()
            .find(|(host, _)| host == "web.feat-1.myapp.localhost")
            .expect("the local hostname is registered");
        let tunnel = entries
            .iter()
            .find(|(host, _)| host == "web-feat-1.myapp.example.com")
            .expect("the tunnel hostname is registered");

        assert_eq!(local.1.endpoint, tunnel.1.endpoint, "the same service");
    }

    #[test]
    fn a_stopped_service_is_wakeable_through_the_tunnel_too() {
        // Scale-to-zero has to work for a reviewer following a shared
        // link, not just for someone on the machine.
        let records = vec![record("feat-1", false)];

        let entries = route_entries(&config(SAMPLE), "myapp", &records, &[], Some("example.com"));

        let tunnel = entries
            .iter()
            .find(|(host, _)| host == "web-feat-1.myapp.example.com")
            .expect("registered while stopped");

        assert!(!tunnel.1.is_running(), "stopped, but known to exist");
    }

    #[test]
    fn no_tunnel_hostnames_without_a_tunnel() {
        let records = vec![record("feat-1", false)];
        let entries = route_entries(&config(SAMPLE), "myapp", &records, &[], None);

        assert!(
            entries.iter().all(|(host, _)| host.ends_with(".localhost")),
            "got: {entries:?}"
        );
    }

    #[test]
    fn the_main_worktree_keeps_its_shorter_tunnel_hostname() {
        // Matching the local URL, where main omits the workspace label.
        let records = vec![record("main", true)];
        let entries = route_entries(&config(SAMPLE), "myapp", &records, &[], Some("example.com"));

        assert!(
            entries
                .iter()
                .any(|(host, _)| host == "web.myapp.example.com"),
            "got: {entries:?}"
        );
    }

    #[test]
    fn unexposed_services_get_no_tunnel_hostname() {
        // A database on the public internet is the accident this exists to
        // avoid.
        let records = vec![record("feat-1", false)];
        let entries = route_entries(&config(SAMPLE), "myapp", &records, &[], Some("example.com"));

        assert!(
            !entries.iter().any(|(host, _)| host.starts_with("db")),
            "got: {entries:?}"
        );
    }

    #[test]
    fn tunnel_urls_are_absent_while_the_tunnel_is_down() {
        // A local URL is worth showing while stopped, because a request
        // starts the service. A tunnel URL with no tunnel behind it has no
        // such recovery — it is simply broken.
        let info = supervisor(Gateway::with_ports(Some(80), Some(443))).build_workspace_info(
            &config(SAMPLE),
            "myapp",
            &record("feat-1", false),
            &[],
        );

        assert!(info.service("web").expect("exists").tunnel_url.is_none());
    }
}
