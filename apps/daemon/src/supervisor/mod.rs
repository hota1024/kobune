//! The lifecycle of a workspace.
//!
//! No human-facing strings go into a response. Progress goes to an
//! [`EventSink`], and how to show it is the CLI's and the GUI's own
//! decision (`docs/DESIGN.md` §3).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;
use minato_api::{
    ApiError, ErrorCode, PurgeProject, PurgeReport, PurgeWorkspace, Request, Response, ServiceInfo,
    Target, Typed, Window, WorkspaceInfo,
};
use minato_core::{MinatoConfig, Paths, ServiceScope, ServiceState, StateStore, WorkspaceRecord};
use minato_proxy::Route;
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
use crate::gateway::Gateway;
use crate::idle::IdleTracker;
use crate::resolve::{self, ProjectContext, Resolved};
use crate::spec;
use crate::tunnel::TunnelHandle;

use self::environment::env_values;
use self::lifecycle::{settle_readiness, validate_service_names};

mod diagnostics;
mod environment;
mod idling;
mod lifecycle;
mod tunnelling;

pub struct Supervisor {
    paths: Paths,
    store: StateStore,
    /// One runtime per `[runtime] default`, reused. Different projects
    /// can then run on different runtimes.
    runtimes: Mutex<HashMap<String, Arc<dyn Runtime>>>,
    /// Serialises writes to the state file.
    ///
    /// **Every `store` access goes under this, without exception.**
    /// [`StateStore::update`] is a load, a mutate and a save with no lock
    /// of its own, so two writers that overlap do not corrupt the file —
    /// they lose one of the two writes, which is worse to diagnose. The
    /// one place that took a different lock and not this one recorded a
    /// completed `setup`, and could erase a workspace that had just been
    /// registered.
    state_lock: Mutex<()>,
    /// Serialises `setup` across the check, the run and the record.
    ///
    /// Its own lock rather than [`Self::state_lock`]: this is held for as
    /// long as an install takes, and holding the state lock that long
    /// would stall every unrelated command. It is held *as well as*, never
    /// instead of — see [`Supervisor::run_setup`].
    ///
    /// **It is why a wave's setups do not overlap**, even though the
    /// starts around them do, and that is the decision rather than a step
    /// not yet taken. Every service mounts the same project-wide cache
    /// volume (`CACHE_VOLUME`, `apps/daemon/src/spec.rs`), so two setups
    /// at once are two arbitrary commands writing into one directory. A
    /// package manager's store is built for that; a `setup` is whatever
    /// somebody wrote, and Minato promises in
    /// `docs/reference/minato-toml.md` that one runs at a time.
    ///
    /// The cost is bounded and falls in one place: the first `up` after
    /// `minato new`. Later ones find the setup recorded and never reach
    /// here.
    ///
    /// Narrowing it to a key would need that promise withdrawn first, and
    /// would want the display to attribute output lines while it was at it
    /// — `apps/cli/src/ui/progress.rs` renders `Event::Output` without the
    /// service the event already names.
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

    /// The runtime for an identifier, built once and reused.
    async fn runtime(&self, id: &str) -> Result<Arc<dyn Runtime>, ApiError> {
        let mut runtimes = self.runtimes.lock().await;

        if let Some(runtime) = runtimes.get(id) {
            return Ok(runtime.clone());
        }

        let runtime: Arc<dyn Runtime> = Arc::from(minato_runtime::create(id, self.paths.root())?);
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
            //
            // **Twice, and the first one is deliberately wrong.** Entering
            // the alternate screen clears it, so a program that is already
            // drawing has to be given a reason to draw again or the
            // attachment lands on a blank screen and stays there until
            // something else happens — which for an event-driven interface
            // may be never. A size that changes is that reason: it is the
            // signal every full-screen program redraws on, and it is why
            // dragging the window has always been the folk cure for a
            // display that came up wrong. Sizing straight to the window
            // would be no change at all for the second person to attach
            // from the same terminal.
            Sizing::Follows(terminal) => {
                let nudge = Window::new(window.cols, window.rows.saturating_sub(1).max(1));
                if nudge != window
                    && let Err(err) = terminal.resize(nudge).await
                {
                    events.debug(format!("cannot size {service}'s terminal: {err}"));
                }

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
            &env_values(&envs),
            &env::workspace_context(&resolved.config, &resolved.workspace, &self.gateway),
        )?;

        workspace_spec
            .service(service)
            .cloned()
            .ok_or_else(|| ApiError::not_found(format!("no service named `{service}`")))
    }

    /// Every project the state store knows about.
    pub async fn known_projects(&self) -> Result<Vec<String>, ApiError> {
        let _guard = self.state_lock.lock().await;
        let state = self.store.load().map_err(ApiError::from)?;
        Ok(state.projects.keys().cloned().collect())
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

        // Everything above worked from the state file, project by project.
        // The storage is swept afterwards and machine-wide, for the reason
        // `purge_volumes` gives.
        let stranded: BTreeSet<String> = report
            .stranded
            .iter()
            .map(|failure| failure.project.clone())
            .collect();

        (report.volumes, report.storage_left) =
            self.purge_volumes(dry_run, &stranded, events).await;

        // The tunnel is machine-wide rather than per project, so it is
        // dealt with once, here.
        report.tunnel = self.purge_tunnel(dry_run, events).await;

        if !dry_run {
            events.step_started("state", "forgetting the projects that are gone");

            // Anything still standing keeps its entry, so a later run can
            // finish the job. Clearing the lot would forget the name of
            // every container that is still up, and there would be nothing
            // left that knew how to find them.
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

    /// Takes the storage Minato made, whichever runtime is holding it.
    ///
    /// **Asked of every runtime, not of the projects in the state file.**
    /// A project volume is deliberately longer-lived than any worktree, so
    /// by the time somebody uninstalls, the state file may have forgotten
    /// the project that owns it — its repository deleted, its worktrees
    /// `minato rm`ed one by one. Sweeping per known project would leave
    /// exactly those behind, under a name Minato chose and nobody else
    /// knows to look for. A runtime that cannot be reached, or was never
    /// installed, has nothing to say and is skipped.
    ///
    /// **A stranded project keeps the storage that keeping it can save.**
    /// Its containers are still up, and taking the data from under them
    /// would be the one irreversible half of a purge that admits it did
    /// not finish — see [`Self::skipping_it_would_save_it`] for the case
    /// where leaving it out of the sweep saves nothing and only hides it.
    ///
    /// **Everything that goes wrong here comes back in the report.** A
    /// runtime that cannot be asked answers exactly as one holding nothing
    /// does, so a failure kept to the log would read as "there is no
    /// storage" all the way out to the plan somebody says yes to.
    async fn purge_volumes(
        &self,
        dry_run: bool,
        stranded: &BTreeSet<String>,
        events: &EventSink,
    ) -> (
        Vec<minato_api::PurgeVolume>,
        Vec<minato_api::PurgeStorageFailure>,
    ) {
        const STEP: &str = "volumes";

        if !dry_run {
            events.step_started(STEP, "removing the storage");
        }

        let mut found = Vec::new();
        let mut left = Vec::new();

        for id in minato_runtime::AVAILABLE_RUNTIMES {
            let Ok(runtime) = self.runtime(id).await else {
                continue;
            };

            let volumes = match runtime.managed_volumes().await {
                Ok(volumes) => volumes,
                Err(err) => {
                    // Not a runtime that is absent — that answers with an
                    // empty list. This is one that is there and would not
                    // say, so whatever it holds is about to be left behind.
                    events.warn(format!("cannot list {id}'s storage: {err}"));
                    left.push(minato_api::PurgeStorageFailure {
                        what: id.to_string(),
                        reason: format!("its storage could not be listed: {err}"),
                    });
                    continue;
                }
            };

            for volume in volumes {
                if stranded.contains(&volume.project) && self.skipping_it_would_save_it(&volume) {
                    continue;
                }

                if !dry_run && let Err(err) = runtime.remove_managed_volume(&volume).await {
                    events.warn(format!("{} was not removed: {err}", volume.id));
                    left.push(minato_api::PurgeStorageFailure {
                        what: volume.id,
                        reason: err.to_string(),
                    });
                    continue;
                }

                found.push(minato_api::PurgeVolume {
                    project: volume.project,
                    name: volume.id,
                });
            }
        }

        found.sort();
        left.sort();

        if !dry_run {
            events.step_done(STEP, "removing the storage");
        }

        (found, left)
    }

    /// Whether leaving this volume out of the sweep would actually keep it.
    ///
    /// Docker's storage is the daemon's to leave alone, so there it would.
    /// Apple Container has no named volumes and uses a directory under
    /// `MINATO_HOME` instead — which the CLI's half of an uninstall deletes
    /// as one entry in its plan, moments after this runs. Skipping it there
    /// saves nothing; all it does is keep the data off the list of what is
    /// about to disappear, which is the one thing that list is for.
    fn skipping_it_would_save_it(&self, volume: &minato_runtime::ManagedVolume) -> bool {
        !std::path::Path::new(&volume.id).starts_with(self.paths.root())
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
                    // Lifted out before the state goes on the wire, where
                    // it becomes a plain string and cannot carry it.
                    reason: state.reason().map(str::to_string),
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
pub(super) mod tests {
    use super::*;
    use crate::gateway::Gateway;
    use std::net::SocketAddr;
    use std::path::Path;

    /// A supervisor for looking at URL building and routing alone. It
    /// never touches the state store, so a path that does not exist is
    /// fine.
    pub(super) fn supervisor(gateway: Gateway) -> Supervisor {
        Supervisor::new(
            &Paths::with_root(PathBuf::from("/tmp/minato-supervisor-test")),
            Arc::new(gateway),
            TunnelHandle::new(),
            Arc::new(tokio::sync::Notify::new()),
        )
    }

    pub(super) fn ready(
        key: minato_runtime::ServiceKey,
        port: u16,
        scope: ServiceScope,
    ) -> ServiceStatus {
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

    pub(super) fn config(toml: &str) -> MinatoConfig {
        let config: MinatoConfig = toml::from_str(toml).expect("is syntactically valid");
        config.validate().expect("is semantically valid");
        config
    }

    /// A graph where the two topological orders differ: `cache` and
    /// `worker` hang off the side of the `web` -> `api` -> `db` chain.
    pub(super) const FORK: &str = r#"
        [project]
        name = "myapp"
        [services.web]
        image = "node:22"
        port = 3000
        depends_on = ["api", "cache"]
        [services.api]
        image = "node:22"
        port = 8080
        depends_on = ["db"]
        [services.worker]
        image = "node:22"
        depends_on = ["db"]
        [services.cache]
        image = "redis:7"
        port = 6379
        [services.db]
        image = "postgres:16"
        port = 5432
    "#;

    pub(super) const SAMPLE: &str = r#"
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

    // What the grouping itself does is pinned on `startup_waves`, in
    // `minato-core`. What is left to test here is the part that only
    // exists at this layer: mapping specs onto those waves, and doing it
    // for a selection narrower than the configuration.

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

    #[test]
    fn a_stranded_project_keeps_the_storage_that_keeping_it_saves() {
        // Docker's volumes are the daemon's to leave alone, so leaving one
        // out of the sweep is what keeps it.
        let supervisor = supervisor(Gateway::inert());

        assert!(
            supervisor.skipping_it_would_save_it(&minato_runtime::ManagedVolume {
                project: "myapp".into(),
                id: "minato-myapp-pgdata".into(),
            })
        );
    }

    #[test]
    fn storage_inside_the_daemons_own_directory_cannot_be_kept() {
        // Apple Container's volumes live under `MINATO_HOME`, which the
        // CLI's half of an uninstall deletes as one entry in its plan.
        // Skipping this would not save the data — it would only leave it
        // off the list of what is about to go, which is the one thing that
        // list exists for.
        let supervisor = supervisor(Gateway::inert());
        let inside = supervisor
            .paths
            .root()
            .join("volumes")
            .join("myapp")
            .join("pgdata");

        assert!(
            !supervisor.skipping_it_would_save_it(&minato_runtime::ManagedVolume {
                project: "myapp".into(),
                id: inside.display().to_string(),
            })
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

    /// A workspace record rooted somewhere that can actually be written.
    pub(super) fn record_at(path: &std::path::Path) -> minato_core::WorkspaceRecord {
        minato_core::WorkspaceRecord {
            path: path.to_path_buf(),
            ..record("feat-1", false)
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
            .find(|(host, _)| host == "web-feat-1-myapp.example.com")
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
            .find(|(host, _)| host == "web-feat-1-myapp.example.com")
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
                .any(|(host, _)| host == "web-myapp.example.com"),
            "got: {entries:?}"
        );
    }

    #[test]
    fn a_tunnel_hostname_is_one_label_under_the_zone() {
        // Universal SSL covers first-level subdomains only, so a second
        // label is refused at Cloudflare's edge with a TLS handshake
        // failure — nothing local goes wrong and nothing local shows it.
        let records = vec![record("feat-1", false)];
        let entries = route_entries(&config(SAMPLE), "myapp", &records, &[], Some("example.com"));

        for (host, _) in entries
            .iter()
            .filter(|(host, _)| host.ends_with(".example.com"))
        {
            let label = host.strip_suffix(".example.com").expect("under the zone");
            assert!(!label.contains('.'), "got: {host}");
        }
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
