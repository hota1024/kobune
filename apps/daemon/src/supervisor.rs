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
    ApiError, Check, Diagnostics, EnvInfo, ErrorCode, Pong, Request, Response, ServiceInfo, Target,
    WorkspaceInfo,
};
use minato_core::{MinatoConfig, Paths, ServiceScope, ServiceState, StateStore, WorkspaceRecord};
use minato_proxy::{Activation, Route};
use minato_runtime::{EventSink, Runtime, ServiceStatus, WorkspaceKey};

/// How the idle sweep groups services: (workspace, service).
type ServiceKeyRef = (String, String);
use tokio::sync::Mutex;

use crate::env;
use crate::gateway::Gateway;
use crate::idle::IdleTracker;
use crate::resolve::{self, ProjectContext, Resolved};
use crate::secrets;
use crate::spec;

pub struct Supervisor {
    paths: Paths,
    store: StateStore,
    /// One runtime per `[runtime] default`, reused. Different projects
    /// can then run on different runtimes.
    runtimes: Mutex<HashMap<String, Arc<dyn Runtime>>>,
    /// Serialises writes to the state file.
    state_lock: Mutex<()>,
    /// The proxy and DNS. Also where URLs come from.
    gateway: Arc<Gateway>,
    /// Last-access times, which is what scale-to-zero decides on.
    idle: IdleTracker,
    started_at: Instant,
    shutdown: Arc<tokio::sync::Notify>,
}

impl Supervisor {
    pub fn new(paths: &Paths, gateway: Arc<Gateway>, shutdown: Arc<tokio::sync::Notify>) -> Self {
        Self {
            paths: paths.clone(),
            store: StateStore::new(paths.state_file()),
            runtimes: Mutex::new(HashMap::new()),
            state_lock: Mutex::new(()),
            gateway,
            idle: IdleTracker::new(),
            started_at: Instant::now(),
            shutdown,
        }
    }

    pub async fn handle(&self, request: Request, events: &EventSink) -> Result<Response, ApiError> {
        match request {
            Request::Ping => self.ping().await,
            Request::Shutdown => {
                self.shutdown.notify_waiters();
                Ok(Response::Empty)
            }
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
            } => {
                self.new_workspace(target, branch, base, path, start, events)
                    .await
            }
            Request::Rm { target, force } => self.rm(target, force, events).await,
            Request::Up { target, services } => self.up(target, services, events).await,
            Request::Down {
                target,
                services,
                all,
            } => self.down(target, services, all, events).await,
            Request::Status { target } => self.status(target).await,
            Request::Doctor => self.doctor().await,
            Request::Logs {
                target,
                services,
                follow,
                tail,
            } => self.logs(target, services, follow, tail, events).await,
            Request::Exec {
                target,
                service,
                command,
            } => self.exec(target, service, command, events).await,
            Request::EnvList { target, reveal } => self.env_list(target, reveal).await,
            Request::EnvSet {
                target,
                scope,
                key,
                value,
            } => self.env_set(target, scope, key, value).await,
            Request::EnvUnset { target, scope, key } => self.env_unset(target, scope, key).await,
        }
    }

    async fn ping(&self) -> Result<Response, ApiError> {
        // Check the default runtime is reachable, and report its version
        // if it is.
        let (runtime_id, version) = match self.runtime("docker").await {
            Ok(runtime) => match runtime.probe().await {
                Ok(info) => (info.id, info.version),
                Err(_) => ("docker".to_string(), "unreachable".to_string()),
            },
            Err(_) => ("docker".to_string(), "unavailable".to_string()),
        };

        Ok(Response::Pong(Pong {
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol: minato_api::PROTOCOL_VERSION,
            runtime: format!("{runtime_id} {version}"),
            uptime_secs: self.started_at.elapsed().as_secs(),
        }))
    }

    /// Diagnoses what the daemon can see.
    ///
    /// System-side settings — `/etc/resolver`, whether the CA is trusted —
    /// are hard to judge from here, so the CLI covers those. This looks at
    /// the listeners and the runtime.
    async fn doctor(&self) -> Result<Response, ApiError> {
        let mut checks = Vec::new();

        // Is the runtime reachable? Nothing starts without it.
        match self.runtime("docker").await {
            Ok(runtime) => match runtime.probe().await {
                Ok(info) => checks.push(Check::ok(
                    "runtime",
                    "container runtime",
                    format!("{} {}", info.id, info.version),
                )),
                Err(err) => checks.push(
                    Check::fail("runtime", "container runtime", err.to_string())
                        .with_fix("start one of Docker Desktop, OrbStack or colima"),
                ),
            },
            Err(err) => checks.push(Check::fail("runtime", "container runtime", err.to_string())),
        }

        checks.push(match self.gateway.http_port() {
            Some(port) => Check::ok("proxy-http", "HTTP proxy", format!("127.0.0.1:{port}")),
            None => Check::fail(
                "proxy-http",
                "HTTP proxy",
                "not listening (the port could not be held)".to_string(),
            )
            .with_fix(
                "a port below 1024 needs privileges. Follow `minato setup`, \
                 or name another port with MINATO_HTTP_PORT",
            ),
        });

        // With only one address family bound, requests to the other reach
        // some different process. Passing over that silently leaves the
        // cause impossible to find.
        let missing = self.gateway.missing_families();
        if !missing.is_empty() {
            let families: Vec<String> = missing.iter().map(|ip| ip.to_string()).collect();
            checks.push(
                Check::fail(
                    "proxy-families",
                    "listening addresses",
                    format!(
                        "{} could not be held. *.localhost resolves to both, \
                         so requests to that address reach another process",
                        families.join(", ")
                    ),
                )
                .with_fix(
                    "stop whatever else is on that address, or name free ports \
                     with MINATO_HTTP_PORT and MINATO_HTTPS_PORT",
                ),
            );
        }

        checks.push(match self.gateway.https_port() {
            Some(port) => Check::ok("proxy-https", "HTTPS proxy", format!("127.0.0.1:{port}")),
            None => Check::warn(
                "proxy-https",
                "HTTPS proxy",
                "not listening; HTTP only".to_string(),
            )
            .with_fix("name another port with MINATO_HTTPS_PORT"),
        });

        checks.push(match self.gateway.dns_port() {
            Some(port) => Check::ok("dns", "DNS server", format!("127.0.0.1:{port}")),
            None => Check::fail(
                "dns",
                "DNS server",
                "not listening; *.localhost will not resolve".to_string(),
            )
            .with_fix("name a port of 1024 or above with MINATO_DNS_PORT"),
        });

        // Whether privileged ports work comes down to whether launchd
        // handed over any descriptors.
        checks.push(if crate::activation::is_active() {
            Check::ok(
                "launchd",
                "launchd socket activation",
                "active (privileged ports are available)".to_string(),
            )
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
        let entries = route_entries(config, project, &records, &statuses);

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

    /// Reads logs and emits them as events.
    ///
    /// Under `follow` this streams until the client disconnects. The
    /// daemon stops the moment a write fails, so nothing has to cancel it
    /// explicitly.
    async fn logs(
        &self,
        target: Target,
        services: Vec<String>,
        follow: bool,
        tail: Option<usize>,
        events: &EventSink,
    ) -> Result<Response, ApiError> {
        let resolved = self.resolve(&target).await?;
        let runtime = self.runtime(&resolved.config.runtime.default).await?;

        let targets = if services.is_empty() {
            resolved.config.services.keys().cloned().collect()
        } else {
            validate_service_names(&resolved.config, &services)?;
            services
        };

        let workspace_key = WorkspaceKey::new(&resolved.project, &resolved.workspace.label);
        let shared_key = WorkspaceKey::shared(&resolved.project);

        let mut streams = Vec::new();
        for name in &targets {
            let service_config = resolved.config.service(name).map_err(ApiError::from)?;
            let key = match service_config.scope {
                ServiceScope::Workspace => workspace_key.service(name),
                ServiceScope::Project => shared_key.service(name),
            };

            match runtime
                .logs(&key, minato_runtime::LogOptions { follow, tail })
                .await
            {
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

    /// Runs a command inside a container.
    async fn exec(
        &self,
        target: Target,
        service: String,
        command: Vec<String>,
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
        let outcome = runtime.exec(&key, &command, events).await?;

        Ok(Response::Exec {
            exit_code: outcome.exit_code,
        })
    }

    /// Shows the environment, layer by layer.
    ///
    /// **Each value says which layer defined it.** With three layers, not
    /// seeing that an unintended one is winning makes the cause impossible
    /// to find.
    async fn env_list(&self, target: Target, reveal: bool) -> Result<Response, ApiError> {
        let resolved = self.resolve(&target).await?;

        // Only the shared layers, without any service's own entries.
        // Per-service differences are `minato status`'s business.
        let layers = env::layers_for_service(
            &resolved.config,
            &resolved.project,
            &resolved.workspace,
            &resolved.repo.main_root,
            // The first service stands in for the rest; everything but
            // MINATO_SERVICE is shared.
            resolved
                .config
                .services
                .keys()
                .next()
                .map(String::as_str)
                .unwrap_or(""),
            &self.paths,
            &self.gateway,
        )
        .map_err(|err| ApiError::new(ErrorCode::InvalidConfig, err.to_string()))?;

        let entries = layers
            .resolve()
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

        Ok(Response::Env { entries })
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

        self.env_list(target, false).await
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

        self.env_list(target, false).await
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
            service,
            &self.paths,
            &self.gateway,
        )
        .map_err(|err| ApiError::new(ErrorCode::InvalidConfig, err.to_string()))?;

        let entries = layers.resolve();

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
            if let Some(route) = self.gateway.routes().get(host) {
                if let Some(endpoint) = route.endpoint {
                    self.idle.touch(host);
                    return Activation::Ready(endpoint);
                }
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
        runtime.prepare(&workspace_spec, &events).await?;

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
        let statuses = self.refresh(&context.project, &context.config).await?;

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
            // The current project only, for now. Covering every project
            // needs the state store to hold the other projects'
            // configuration paths first.
            tracing::debug!("all_projects still returns only the current project");
        }

        Ok(Response::Workspaces { workspaces })
    }

    async fn status(&self, target: Target) -> Result<Response, ApiError> {
        let resolved = self.resolve(&target).await?;
        let statuses = self.refresh(&resolved.project, &resolved.config).await?;

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
        branch: String,
        base: Option<String>,
        path: Option<PathBuf>,
        start: bool,
        events: &EventSink,
    ) -> Result<Response, ApiError> {
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
            self.start_services(&resolved, &[], events).await?;
        }

        let statuses = self.refresh(&resolved.project, &resolved.config).await?;

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

    async fn up(
        &self,
        target: Target,
        services: Vec<String>,
        events: &EventSink,
    ) -> Result<Response, ApiError> {
        let resolved = self.resolve(&target).await?;
        self.start_services(&resolved, &services, events).await?;

        let statuses = self.refresh(&resolved.project, &resolved.config).await?;

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
        events: &EventSink,
    ) -> Result<(), ApiError> {
        let runtime = self.runtime(&resolved.config.runtime.default).await?;

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

        runtime.prepare(&prepare_spec, events).await?;

        // Started in startup_order.
        for service in &filtered {
            runtime.start(service, events).await?;
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

        let statuses = self.refresh(&resolved.project, &resolved.config).await?;

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

                ServiceInfo {
                    name: name.clone(),
                    state,
                    scope: service_config.scope,
                    url,
                    tunnel_url: None,
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

        let entries = route_entries(&config(SAMPLE), "myapp", &records, &statuses);

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

        let entries = route_entries(&config(SAMPLE), "myapp", &records, &statuses);
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
}
