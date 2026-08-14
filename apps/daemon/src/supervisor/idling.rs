//! Scale-to-zero: what a request wakes, and what silence stops.
//!
//! **A request is the only way in.** Only a service with a URL can be
//! named by one, so waking follows `depends_on` outwards from there, and
//! stopping reads the same edges backwards — an `expose = false` service
//! has no last access of its own and takes its dependents'. Both halves
//! are one rule seen from either side, and holding them apart is what
//! left a database running for as long as the daemon did.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::time::Duration;

use kobune_api::ApiError;
use kobune_core::config::{KobuneConfig, ServiceScope};
use kobune_proxy::{Activation, Route};
use kobune_runtime::{EventSink, Runtime, WorkspaceKey};

use crate::env;
use crate::spec;

use super::environment::write_env_file_for;
use super::lifecycle::{run_wave, select_with_dependencies, waves};
use super::{ServiceKeyRef, Supervisor};

impl Supervisor {
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
    /// its `kobune.toml` may have moved, or the runtime may be down, and
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
    /// Starts the service behind a host, and everything it depends on.
    ///
    /// **The dependencies come too.** A request is what wakes a service,
    /// and only a service with a URL can be woken that way — so a `db`
    /// behind `expose = false` has no request of its own to arrive.
    /// Starting the one service named by the host would hand the app a
    /// dependency that is not there, which is the failure `depends_on`
    /// exists to prevent. `up` has always done this; the wake path used
    /// to be the one way in that did not.
    ///
    /// Under Apple Container it decides more than ordering:
    /// `KOBUNE_HOST_<SERVICE>` carries a peer's address, read after the
    /// peer has started, so a service woken on its own gets no variable
    /// at all.
    async fn start_for_host(
        &self,
        host: &str,
        route: &Route,
    ) -> Result<Option<SocketAddr>, ApiError> {
        let (config, record) = self.locate(&route.project, &route.workspace).await?;

        let events = EventSink::discard();
        let project_root = self.project_root(&route.project).await?;
        let context = env::workspace_context(&config, &record, &self.gateway);

        // Dependencies first, so a peer is up before whatever needs it.
        let starting = wake_order(&config, &route.service)?;

        let mut specs = Vec::with_capacity(starting.len());
        for name in &starting {
            let service_config = config.service(name).map_err(ApiError::from)?;
            let service_env = self
                .service_env(
                    &config,
                    &route.project,
                    &record,
                    &project_root,
                    name,
                    &events,
                )
                .await?;

            // Only what is starting, exactly as `up` does it.
            write_env_file_for(&config, &record, name, &service_env)?;

            specs.push(spec::build_service_spec(
                service_config,
                name,
                &route.project,
                &record.label,
                &record.path,
                service_env.values,
                &context,
            )?);
        }

        let runtime = self.runtime(&config.runtime.default).await?;

        // start fails without the image, so prepare them all first.
        let workspace_spec = kobune_runtime::WorkspaceSpec {
            key: WorkspaceKey::new(&route.project, &record.label),
            worktree_path: record.path.clone(),
            services: specs.clone(),
        };
        // Never a forced rebuild: this sits in the path of the request
        // that woke the service, and the fingerprint in the tag already
        // means an existing image was built from these inputs.
        runtime.prepare(&workspace_spec, false, &events).await?;

        tracing::info!("a request to {host} is starting {}", starting.join(", "));

        // A wave at a time, as `up` does. This one is on the path of a
        // request that is being held open, so the wait saved is a wait
        // somebody is sitting through.
        let concurrently = runtime.starts_concurrently();
        let mut endpoint = None;
        for wave in waves(&config, &specs, concurrently)? {
            let started = run_wave(
                concurrently,
                wave.iter()
                    .map(|spec| runtime.start(spec, &events))
                    .collect(),
            )
            .await?;

            // The host was asked for one service. The rest are here to
            // stand behind it.
            if let Some(running) = started
                .into_iter()
                .find(|running| running.key.service == route.service)
            {
                endpoint = running.endpoint;
            }
        }

        self.refresh(&route.project, &config).await?;
        Ok(endpoint)
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
        //
        // Only exposed services get this far: [`Route`] is the only thing
        // read here, and an unexposed service has none. They are swept
        // separately, by [`Self::sweep_internal`].
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
        // The hosts this sweep took down, which the snapshot it started
        // from still calls running.
        let mut just_stopped: BTreeSet<String> = BTreeSet::new();

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
                just_stopped.insert(host.clone());
            }
            stopped += 1;
        }

        // **After the exposed ones**, and told what they were. A service
        // stopped a moment ago is stopped, whatever the snapshot this
        // sweep started from says — and without saying so, an internal
        // service would wait another whole sweep to follow its last
        // dependent down.
        let settled: Vec<(String, Route)> = routes
            .iter()
            .map(|(host, route)| {
                let route = if just_stopped.contains(host) {
                    Route::stopped(&route.project, &route.workspace, &route.service)
                } else {
                    route.clone()
                };

                (host.clone(), route)
            })
            .collect();

        stopped += self
            .sweep_internal(project, &config, runtime.as_ref(), &settled, &events)
            .await?;

        if stopped > 0 {
            self.refresh(project, &config).await?;
        }

        Ok(stopped)
    }
    /// Stops the internal services nothing is reaching for any more.
    ///
    /// A service with `expose = false` has no URL, so no request ever
    /// names it and it has no last access of its own to measure. Left at
    /// that it was never a candidate at all, and a database started once
    /// stayed up for as long as the daemon did — which is the opposite of
    /// what makes a worktree cheap to create.
    ///
    /// It reads its exposed dependents instead. One that is stopped is
    /// plainly not using it; one that is running is judged on its own
    /// last access.
    ///
    /// **With no exposed dependent it is left alone.** Waking follows
    /// `depends_on` outwards from a service a request can reach
    /// ([`Self::start_for_host`]), so an internal service nothing depends
    /// on has no way back up, and stopping it would be one-way.
    async fn sweep_internal(
        &self,
        project: &str,
        config: &KobuneConfig,
        runtime: &dyn Runtime,
        routes: &[(String, Route)],
        events: &EventSink,
    ) -> Result<usize, ApiError> {
        let candidates = unreached_internal_services(
            project,
            config,
            routes,
            &|host| self.idle.idle_for(host),
            &|host| self.idle.is_starting(host),
        );

        if candidates.is_empty() {
            return Ok(0);
        }

        // An internal service has no route, so the runtime is the only
        // place its state can come from. Asked for only once something
        // looks worth stopping, so a project without any stays free.
        let statuses = runtime.list_project(project).await?;

        let mut stopped = 0;
        for service_key in candidates {
            let running = statuses
                .iter()
                .any(|status| status.key == service_key && status.state.is_running());

            if !running {
                continue;
            }

            // As for an exposed service: an open terminal sends no
            // requests, and is the one kind of use nothing else sees.
            if self.idle.is_in_use(&service_key.to_string()) {
                continue;
            }

            tracing::info!("stopping {service_key} (nothing that depends on it is in use)");

            if let Err(err) = runtime.stop(&service_key, events).await {
                tracing::warn!("cannot stop {service_key}: {err}");
                continue;
            }

            stopped += 1;
        }

        Ok(stopped)
    }
}

/// What waking `service` has to start, dependencies first.
///
/// [`select_with_dependencies`] answers *which*; `startup_order` answers
/// *in what order*. `up` gets the second for free by filtering a spec that
/// is already ordered, and the wake path has no such spec to filter.
fn wake_order(config: &KobuneConfig, service: &str) -> Result<Vec<String>, ApiError> {
    let needed = select_with_dependencies(config, std::slice::from_ref(&service.to_string()))?;

    Ok(config
        .startup_order()
        .into_iter()
        .filter(|name| needed.iter().any(|needed| needed == name))
        .map(str::to_string)
        .collect())
}
/// The internal services nothing has reached for within their timeout.
///
/// Pure, and so testable without a runtime to ask. Whether one of these
/// is actually up is a separate question, and the only one that needs
/// the runtime — see [`Supervisor::sweep_internal`].
///
/// A dependent that is stopped counts as idle rather than as unknown.
/// It is not sending requests by definition, and its last access was
/// forgotten when it stopped, so reading the absence of a time as "still
/// in use" is what would keep a database up behind a whole workspace
/// that has already gone quiet.
fn unreached_internal_services(
    project: &str,
    config: &KobuneConfig,
    routes: &[(String, Route)],
    idle_for: &dyn Fn(&str) -> Option<Duration>,
    starting: &dyn Fn(&str) -> bool,
) -> Vec<kobune_runtime::ServiceKey> {
    let internal: Vec<&str> = config
        .services
        .iter()
        .filter(|(_, service)| !service.exposed())
        .map(|(name, _)| name.as_str())
        .collect();

    if internal.is_empty() {
        return Vec::new();
    }

    // What each exposed service pulls up with it, worked out once rather
    // than once per pairing.
    let mut needs: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for (_, route) in routes {
        if needs.contains_key(route.service.as_str()) {
            continue;
        }

        if let Ok(order) = wake_order(config, &route.service) {
            needs.insert(route.service.as_str(), order);
        }
    }

    let mut unreached = Vec::new();
    for name in internal {
        let Ok(service_config) = config.service(name) else {
            continue;
        };
        let timeout = service_config.idle_timeout();

        // Grouped the way the service is keyed: one instance per
        // workspace, or one shared by the whole project.
        let mut dependents: BTreeMap<String, Vec<&(String, Route)>> = BTreeMap::new();
        for entry in routes {
            let (_, route) = entry;

            if !needs
                .get(route.service.as_str())
                .is_some_and(|needed| needed.iter().any(|needed| needed == name))
            {
                continue;
            }

            let key = match service_config.scope {
                ServiceScope::Workspace => route.workspace.clone(),
                ServiceScope::Project => String::new(),
            };

            dependents.entry(key).or_default().push(entry);
        }

        for (workspace, entries) in dependents {
            // One dependent still in use is enough to keep it up.
            let all_idle = entries.iter().all(|(host, route)| {
                // Being woken right now. Its route has no endpoint yet,
                // so reading "not running" as "not using it" would stop
                // the database out from under the request doing the
                // waking.
                if starting(host) {
                    return false;
                }

                !route.is_running() || idle_for(host).is_some_and(|idle| idle >= timeout)
            });

            if !all_idle {
                continue;
            }

            unreached.push(match service_config.scope {
                ServiceScope::Workspace => WorkspaceKey::new(project, &workspace).service(name),
                ServiceScope::Project => WorkspaceKey::shared(project).service(name),
            });
        }
    }

    unreached
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::tests::{SAMPLE, config};
    use std::net::SocketAddr;

    #[test]
    fn waking_a_service_starts_its_dependencies_first() {
        // A request is what wakes a service, and only an exposed one has
        // a URL for a request to name. Starting `web` alone would hand it
        // an `api` and a `db` that are not there.
        let config = config(SAMPLE);
        let order = wake_order(&config, "web").expect("resolves");

        assert_eq!(order, vec!["db", "api", "web"]);
    }
    #[test]
    fn waking_a_service_leaves_out_what_it_does_not_need() {
        // The cold-start path sits in front of somebody's request, so it
        // starts what the host needs and nothing else.
        let config = config(SAMPLE);
        let order = wake_order(&config, "api").expect("resolves");

        assert_eq!(
            order,
            vec!["db", "api"],
            "`web` depends on api, not the other way"
        );
    }
    /// An exposed route, running or not.
    fn route_for(workspace: &str, service: &str, running: bool) -> (String, Route) {
        let host = format!("{service}.{workspace}.myapp.localhost");
        let route = if running {
            Route::new(
                SocketAddr::from(([127, 0, 0, 1], 3000)),
                "myapp",
                workspace,
                service,
            )
        } else {
            Route::stopped("myapp", workspace, service)
        };

        (host, route)
    }
    /// Every host answered as idle for this long.
    fn idle_by(seconds: u64) -> impl Fn(&str) -> Option<Duration> {
        move |_| Some(Duration::from_secs(seconds))
    }
    /// Nothing is being woken.
    fn never_starting(_: &str) -> bool {
        false
    }
    #[test]
    fn an_internal_service_stops_once_its_dependents_go_quiet() {
        // `db` has no URL, so no request ever names it and it has no last
        // access of its own. Read literally that made it a candidate
        // never — one database per worktree, up for as long as the daemon
        // was.
        let config = config(SAMPLE);
        let routes = vec![
            route_for("feat-1", "web", true),
            route_for("feat-1", "api", true),
        ];

        let unreached = unreached_internal_services(
            "myapp",
            &config,
            &routes,
            &idle_by(31 * 60), // past the 30m default
            &never_starting,
        );

        assert_eq!(
            unreached,
            vec![WorkspaceKey::shared("myapp").service("db")],
            "db is shared, so it is keyed to the project"
        );
    }
    #[test]
    fn one_busy_dependent_keeps_an_internal_service_up() {
        let config = config(SAMPLE);
        let routes = vec![
            route_for("feat-1", "web", true),
            route_for("feat-1", "api", true),
        ];

        let unreached = unreached_internal_services(
            "myapp",
            &config,
            &routes,
            &|host| {
                // `api` is still being called; `web` has gone quiet.
                Some(Duration::from_secs(if host.starts_with("api.") {
                    5
                } else {
                    31 * 60
                }))
            },
            &never_starting,
        );

        assert!(
            unreached.is_empty(),
            "a database is shared, and one caller is enough to need it"
        );
    }
    #[test]
    fn a_stopped_dependent_counts_as_idle_rather_than_unknown() {
        // Its last access was forgotten when it stopped, so there is no
        // time to read. Treating that as "still in use" is exactly what
        // kept the database up behind a workspace that had gone.
        let config = config(SAMPLE);
        let routes = vec![
            route_for("feat-1", "web", false),
            route_for("feat-1", "api", false),
        ];

        let unreached =
            unreached_internal_services("myapp", &config, &routes, &|_| None, &never_starting);

        assert_eq!(unreached, vec![WorkspaceKey::shared("myapp").service("db")]);
    }
    #[test]
    fn an_internal_service_nothing_depends_on_is_left_alone() {
        // Waking follows `depends_on` outwards from a service a request
        // can reach. With nothing pointing at it there is no way back up,
        // so stopping it would be one-way.
        let config = config(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            port = 3000
            [services.db]
            image = "postgres:16"
            port = 5432
            expose = false
        "#,
        );

        let routes = vec![route_for("feat-1", "web", true)];
        let unreached = unreached_internal_services(
            "myapp",
            &config,
            &routes,
            &idle_by(31 * 60),
            &never_starting,
        );

        assert!(unreached.is_empty(), "nothing would ever start it again");
    }
    #[test]
    fn a_workspace_scoped_internal_service_is_decided_per_worktree() {
        // Unlike the shared one, each worktree has its own — so one
        // worktree still working must not keep the other's up.
        let config = config(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            port = 3000
            depends_on = ["db"]
            [services.db]
            image = "postgres:16"
            port = 5432
            expose = false
        "#,
        );

        let routes = vec![
            route_for("feat-1", "web", true),
            route_for("feat-2", "web", true),
        ];

        let unreached = unreached_internal_services(
            "myapp",
            &config,
            &routes,
            &|host| {
                Some(Duration::from_secs(if host.contains("feat-2") {
                    5
                } else {
                    31 * 60
                }))
            },
            &never_starting,
        );

        assert_eq!(
            unreached,
            vec![WorkspaceKey::new("myapp", "feat-1").service("db")],
            "only the quiet worktree's database"
        );
    }
    #[test]
    fn a_dependent_being_woken_holds_it_open() {
        // The narrow one. A host part-way through a wake has no endpoint
        // on its route yet, so it looks stopped — and stopped counts as
        // idle. Read literally, a sweep landing inside that window would
        // stop the database out from under the request that is starting
        // it, and the app would come up against nothing.
        let config = config(SAMPLE);
        let routes = vec![
            route_for("feat-1", "web", false),
            route_for("feat-1", "api", false),
        ];

        let unreached =
            unreached_internal_services("myapp", &config, &routes, &|_| None, &|host| {
                host.starts_with("web.")
            });

        assert!(
            unreached.is_empty(),
            "a start in flight is use, whatever the route says yet"
        );
    }
    #[test]
    fn an_exposed_service_is_not_swept_twice() {
        // The exposed half of the sweep already owns these, and stopping
        // one from both places would ask the runtime twice and count it
        // twice.
        let config = config(SAMPLE);
        let routes = vec![route_for("feat-1", "web", true)];

        let unreached = unreached_internal_services(
            "myapp",
            &config,
            &routes,
            &idle_by(31 * 60),
            &never_starting,
        );

        assert!(
            unreached.iter().all(|key| key.service == "db"),
            "got: {unreached:?}"
        );
    }
}
