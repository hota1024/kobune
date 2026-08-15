//! Starting and stopping the services of a workspace.
//!
//! **Dependencies decide the order, and the runtime decides the width.**
//! `waves` groups what can go at once; a runtime that cannot start two
//! things concurrently gets waves of one, in `startup_order`. `setup`
//! runs interleaved rather than in a batch first, because a migration
//! against `db` has to run after `db` is up.

use std::time::Duration;

use kobune_api::{ApiError, ErrorCode, Response, Target};
use kobune_core::config::KobuneConfig;
use kobune_core::{HealthCheck, ServiceScope, ServiceState};
use kobune_runtime::{EventSink, Runtime, ServiceStatus, WorkspaceKey};

use crate::env;
use crate::resolve::Resolved;
use crate::spec;

use super::Supervisor;
use super::environment::{env_values, write_env_files};

impl Supervisor {
    pub(super) async fn up(
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
    pub(super) async fn start_services(
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
        // no URL to hand out, so `KOBUNE_URL_<SERVICE>` is left unset — and
        // inside the container that surfaces as `parameter not set` from a
        // start-up script, which names nothing that leads back to here.
        if !self.gateway.is_serving() {
            events.warn(
                "the proxy is not listening, so no KOBUNE_URL_<SERVICE> is \
                 injected and the URLs will not answer. `kobune doctor` says \
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
            &env_values(&envs),
            &env::workspace_context(&resolved.config, &resolved.workspace, &self.gateway),
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

        // **Before anything is prepared**, so a file that cannot be
        // written stops the start rather than being discovered by a
        // container that read the old one. Only the selected services:
        // `kobune up web` has no business failing over what `api` asks
        // for, or writing into a path `api` alone was pointed at.
        write_env_files(&resolved.config, &resolved.workspace, &envs, &selected)?;

        let prepare_spec = kobune_runtime::WorkspaceSpec {
            key: workspace_spec.key.clone(),
            worktree_path: workspace_spec.worktree_path.clone(),
            services: filtered.clone(),
        };

        runtime.prepare(&prepare_spec, rebuild, events).await?;

        // Started a wave at a time, each service set up just before it
        // starts.
        //
        // **Interleaved, not done in a batch first.** A setup that needs a
        // dependency — migrations against `db` — has to run after the
        // thing it depends on is up, and the waves already put them in
        // that order.
        let concurrently = runtime.starts_concurrently();
        for wave in waves(&resolved.config, &filtered, concurrently)? {
            run_wave(
                concurrently,
                wave.iter()
                    .map(|service| {
                        self.setup_and_start(resolved, service, runtime.as_ref(), events)
                    })
                    .collect(),
            )
            .await?;
        }

        Ok(())
    }
    /// One service's `setup`, then the service.
    async fn setup_and_start(
        &self,
        resolved: &Resolved,
        service: &kobune_runtime::ServiceSpec,
        runtime: &dyn Runtime,
        events: &EventSink,
    ) -> Result<(), ApiError> {
        self.run_setup(resolved, service, runtime, events).await?;
        runtime.start(service, events).await?;
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
        spec: &kobune_runtime::ServiceSpec,
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
        //
        // It does not stand in for [`Self::state_lock`], which is what
        // makes a read-modify-write of the state file safe against
        // everything that is not a setup. Both are taken, this one first —
        // an ordering nothing else can contradict, because `setup_lock` is
        // taken here and nowhere else.
        let _guard = self.setup_lock.lock().await;

        let project = resolved.project.clone();
        let workspace = resolved.workspace.label.clone();

        let pending = {
            let _state = self.state_lock.lock().await;
            self.store.load().map_err(ApiError::from)?.needs_setup(
                &project,
                &workspace,
                name,
                service.scope,
                &setup,
            )
        };

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
            .with_hint("the output above says what happened. Fix it and run `kobune up` again"));
        }

        events.step_done(&step, &label);

        // Recorded only once it has worked. A setup that failed has not
        // run, whatever it managed to do before giving up.
        let service_name = name.to_string();
        let scope = service.scope;
        let recorded = {
            let _state = self.state_lock.lock().await;
            self.store
                .update(|state| {
                    Ok(state.record_setup(&project, &workspace, &service_name, scope, &setup))
                })
                .map_err(ApiError::from)?
        };

        if !recorded {
            events.debug(format!(
                "{name} was set up, but the workspace went before it could be remembered"
            ));
        }

        Ok(())
    }
    pub(super) async fn down(
        &self,
        target: Target,
        services: Vec<String>,
        all: bool,
        events: &EventSink,
    ) -> Result<Response, ApiError> {
        let resolved = self.resolve(&target).await?;
        let runtime = self.runtime(&resolved.config.runtime.default).await?;

        if all {
            // Stop every Kobune-managed service in the project.
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
/// Not [`kobune_runtime::DEFAULT_READINESS_TIMEOUT`], which is how long
/// *starting* waits for an app to come up. This is a question asked while
/// someone waits for the answer, and a check that has not replied over
/// loopback by now is not serving. Reporting `starting` after a second
/// beats making `kobune status` sit there.
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
pub(super) async fn settle_readiness(config: &KobuneConfig, statuses: &mut [ServiceStatus]) {
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
                    kobune_runtime::probe(endpoint, Some(&health), None),
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
/// Runs one wave's worth of work, overlapping it where that is allowed.
///
/// **The error that comes back is the first in wave order**, either way,
/// so which one it is does not depend on which service happened to give up
/// first.
///
/// Concurrently, the whole wave finishes before a failure is reported. Its
/// members are independent by construction, so one failing says nothing
/// about the others, and abandoning them mid-flight would leave containers
/// half-created for no gain. In sequence there is nothing already in
/// flight to see out, so the first failure stops it — which is what this
/// path has always done.
///
/// **Only one error can be returned, and a wave can produce several.** The
/// rest have already gone out as `step_failed`, so the client has seen
/// them; they are logged here too, because the response alone would say
/// one service failed when more did.
pub(super) async fn run_wave<T, E: std::fmt::Display>(
    concurrently: bool,
    work: Vec<impl std::future::Future<Output = Result<T, E>>>,
) -> Result<Vec<T>, E> {
    if concurrently {
        let outcomes = futures::future::join_all(work).await;

        for also in outcomes
            .iter()
            .filter_map(|outcome| outcome.as_ref().err())
            .skip(1)
        {
            tracing::warn!("another service in the same wave failed: {also}");
        }

        return outcomes.into_iter().collect();
    }

    let mut done = Vec::with_capacity(work.len());
    for one in work {
        done.push(one.await?);
    }

    Ok(done)
}
/// Regroups an ordered run of specs into the waves they can start in.
///
/// `startup_waves` has already decided the grouping; this only picks out
/// the specs the caller is actually starting. A wave the selection skipped
/// entirely is dropped rather than left empty, which keeps "one wave, one
/// round of starts" true.
///
/// **A runtime that starts in sequence is not regrouped at all.** Grouping
/// by depth is a different topological order from `startup_order` — both
/// are correct, but they are not the same list, and flattening one back
/// out does not recover the other. That distinction is invisible where a
/// wave starts at once and decisive where it does not: Apple Container
/// reads a peer's address off whatever is running when a container is
/// created, and `peers` is every other service in the workspace rather
/// than only `depends_on`, so a service reordered past a neighbour is
/// handed a different set of `KOBUNE_HOST_<PEER>` variables. Sequential
/// backends therefore keep the order they have always had.
///
/// **Depth is read off the whole configuration, not the selection.** That
/// is the same answer, because a selection always arrives closed over its
/// dependencies (see [`select_with_dependencies`]) — so every dependency
/// a selected service has is selected too, and sits in the wave the full
/// graph puts it in.
pub(super) fn waves<'a>(
    config: &KobuneConfig,
    specs: &'a [kobune_runtime::ServiceSpec],
    concurrently: bool,
) -> Result<Vec<Vec<&'a kobune_runtime::ServiceSpec>>, ApiError> {
    if !concurrently {
        return Ok(specs.iter().map(|spec| vec![spec]).collect());
    }

    let grouped: Vec<Vec<&kobune_runtime::ServiceSpec>> = config
        .startup_waves()
        .into_iter()
        .map(|wave| {
            specs
                .iter()
                .filter(|spec| wave.contains(&spec.name()))
                .collect()
        })
        .filter(|wave: &Vec<_>| !wave.is_empty())
        .collect();

    // Every spec was built by walking this same configuration — through
    // `build_workspace_spec` or through `wake_order`, both of which read
    // `startup_order` — so `startup_waves` names all of them.
    //
    // **Said out loud rather than left to a `debug_assert`.** The daemon
    // people run is a release build, and there the assertion is compiled
    // out: a spec that fell through would simply never be started, and
    // `up` would report success for a service that is not running.
    let placed: usize = grouped.iter().map(Vec::len).sum();
    if placed != specs.len() {
        return Err(ApiError::new(
            ErrorCode::Internal,
            "some services could not be ordered against the configuration \
             they came from"
                .to_string(),
        ));
    }

    Ok(grouped)
}
/// The named services, plus everything they depend on.
pub(super) fn select_with_dependencies(
    config: &KobuneConfig,
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
pub(super) fn validate_service_names(
    config: &KobuneConfig,
    names: &[String],
) -> Result<(), ApiError> {
    for name in names {
        if !config.services.contains_key(name) {
            let available: Vec<&str> = config.services.keys().map(String::as_str).collect();
            return Err(ApiError::not_found(format!("no service named `{name}`"))
                .with_hint(format!("available: {}", available.join(", "))));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::tests::{FORK, SAMPLE, config, ready};
    use kobune_core::{ServiceScope, ServiceState};
    use std::collections::BTreeMap;
    use std::path::Path;

    /// A config whose only service is `web`, so `ready(...)` lines up.
    ///
    /// `health` is what makes readiness answerable from outside the
    /// container, so it is the shape worth testing against.
    fn web_with_http_health() -> KobuneConfig {
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
    fn web_only() -> KobuneConfig {
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
        // which is the question `kobune status` is for.
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
    /// The specs `up` would start, for the services `only` names.
    ///
    /// Built the way `start_services` builds them — ordered, then narrowed
    /// to the selection — so what the wave tests are handed is what the
    /// real path hands `waves`.
    fn specs_for(config: &KobuneConfig, only: &[&str]) -> Vec<kobune_runtime::ServiceSpec> {
        let names: Vec<String> = only.iter().map(|name| (*name).to_string()).collect();
        let selected = select_with_dependencies(config, &names).expect("resolves");

        let context = crate::spec::WorkspaceContext {
            services: config.services.keys().cloned().collect(),
            gateway_hosts: vec![],
            ca_file: None,
        };

        spec::build_workspace_spec(
            config,
            "myapp",
            "feat-1",
            Path::new("/repo/wt/feat-1"),
            &BTreeMap::new(),
            &context,
        )
        .expect("builds")
        .services
        .into_iter()
        .filter(|spec| selected.iter().any(|name| name == spec.name()))
        .collect()
    }
    fn wave_names<'a>(waves: &[Vec<&'a kobune_runtime::ServiceSpec>]) -> Vec<Vec<&'a str>> {
        waves
            .iter()
            .map(|wave| wave.iter().map(|spec| spec.name()).collect())
            .collect()
    }
    #[test]
    fn a_narrowed_selection_leaves_no_empty_wave_behind() {
        // `up api` skips `web` entirely. `web` sits in wave 2 of the full
        // graph, and an empty wave 2 left in place would be a round of
        // starts that starts nothing.
        let config = config(SAMPLE);
        let specs = specs_for(&config, &["api"]);

        assert_eq!(
            wave_names(&waves(&config, &specs, true).expect("orders")),
            vec![vec!["db"], vec!["api"]]
        );
    }
    #[test]
    fn a_wave_never_holds_a_service_and_its_dependency() {
        // The one invariant the whole thing rests on: everything in a wave
        // starts at once, so a dependency sharing a wave with what depends
        // on it would be the ordering `depends_on` exists to give.
        let config = config(FORK);
        let specs = specs_for(&config, &[]);
        let grouped = waves(&config, &specs, true).expect("orders");

        let mut started: Vec<&str> = Vec::new();
        for wave in &grouped {
            for spec in wave {
                for dependency in &config.services[spec.name()].depends_on {
                    assert!(
                        started.contains(&dependency.as_str()),
                        "{} starts alongside or before {dependency}, which it depends on: {:?}",
                        spec.name(),
                        wave_names(&grouped)
                    );
                }
            }
            started.extend(wave.iter().map(|spec| spec.name()));
        }

        assert_eq!(
            started.len(),
            config.services.len(),
            "every service still starts: {:?}",
            wave_names(&grouped)
        );
    }
    #[test]
    fn a_sequential_runtime_starts_in_startup_order() {
        // The regression this file's `waves` argument exists for. Grouping
        // by depth is a different topological order, and a backend that
        // opted out of concurrency did so to keep the order it had —
        // Apple Container reads a peer's address off whatever is already
        // running, so being reordered past a neighbour changes what a
        // service is told about it.
        let config = config(FORK);
        let specs = specs_for(&config, &[]);

        let sequential = wave_names(&waves(&config, &specs, false).expect("orders"));

        assert_eq!(
            sequential.concat(),
            config.startup_order(),
            "a sequential backend must see exactly what it saw before"
        );
        assert!(
            sequential.iter().all(|wave| wave.len() == 1),
            "and one at a time: {sequential:?}"
        );

        // Worth stating beside it: the concurrent grouping really is a
        // different list, so the two branches are not interchangeable.
        assert_ne!(
            wave_names(&waves(&config, &specs, true).expect("orders")).concat(),
            config.startup_order()
        );
    }
    #[tokio::test]
    async fn a_wave_reports_the_first_failure_in_wave_order() {
        // Both branches, because the promise is that which error comes
        // back does not depend on which future happened to finish first.
        let failing = |names: [&'static str; 3]| {
            names.map(|name| async move {
                match name {
                    "ok" => Ok(name),
                    _ => Err(ApiError::new(ErrorCode::RuntimeFailed, name.to_string())),
                }
            })
        };

        for concurrently in [true, false] {
            let outcome = run_wave(
                concurrently,
                failing(["ok", "second", "third"]).into_iter().collect(),
            )
            .await;

            assert_eq!(
                outcome.expect_err("one of them failed").message,
                "second",
                "concurrently = {concurrently}"
            );
        }
    }
    #[tokio::test]
    async fn a_wave_that_works_keeps_what_it_produced_in_order() {
        for concurrently in [true, false] {
            let work = ["a", "b", "c"].map(|name| async move { Ok::<_, ApiError>(name) });

            assert_eq!(
                run_wave(concurrently, work.into_iter().collect())
                    .await
                    .expect("all fine"),
                vec!["a", "b", "c"],
                "concurrently = {concurrently}"
            );
        }
    }
    #[test]
    fn unknown_service_lists_the_available_ones() {
        let config = config(SAMPLE);
        let err = select_with_dependencies(&config, &["nope".to_string()]).unwrap_err();

        assert_eq!(err.code, ErrorCode::NotFound);
        let hint = err.hint.expect("has a hint");
        assert!(hint.contains("web") && hint.contains("api"), "got: {hint}");
    }
}
