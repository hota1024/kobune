//! The Docker backend.
//!
//! Talks to the Docker API directly rather than shelling out to `docker
//! compose`. Building on compose would cap the design at "whatever compose
//! can do", and the other runtimes could not follow.
//!
//! Ports are forwarded to a dynamically assigned port on `127.0.0.1`.
//! Exposing them on `0.0.0.0` would put the development environment in
//! front of everyone else on the LAN.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bollard::Docker;
use bollard::container::{
    AttachContainerOptions, Config, CreateContainerOptions, ListContainersOptions, LogOutput,
    LogsOptions, RemoveContainerOptions, ResizeContainerTtyOptions, StartContainerOptions,
    StopContainerOptions,
};
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::image::{BuildImageOptions, CreateImageOptions};
use bollard::models::{
    ContainerSummary, EndpointSettings, HostConfig, Mount, MountTypeEnum, PortBinding,
};
use bollard::network::{ConnectNetworkOptions, CreateNetworkOptions, ListNetworksOptions};
use bollard::volume::ListVolumesOptions;
use futures::StreamExt;
use futures::stream::BoxStream;
use kobune_api::OutputStream;
use kobune_core::{Modes, ServiceScope, ServiceState};

use crate::error::{Result, RuntimeError};
use crate::event::EventSink;
use crate::health::{DEFAULT_READINESS_TIMEOUT, await_service};
use crate::runtime::{
    Attachment, DEFAULT_WINDOW, ExecOptions, ExecOutcome, LogLine, LogOptions, Resize, Runtime,
    RuntimeInfo, Sizing, Throwaway, labels, names,
};
use crate::spec::{
    BuildSpec, ManagedVolume, RunningService, ServiceKey, ServiceSpec, ServiceStatus, SourceMount,
    VolumeMount, WorkspaceKey, WorkspaceSpec,
};

const RUNTIME_ID: &str = "docker";

/// Runs a `cmd:` health check inside a container.
///
/// Its own path rather than [`DockerRuntime::exec`]: that one streams output
/// as events, and a check running every 100ms would drown the log.
struct DockerCommandProbe {
    docker: Docker,
    container: String,
}

#[async_trait]
impl crate::health::CommandProbe for DockerCommandProbe {
    async fn succeeds(&self, command: &[String]) -> bool {
        let created = self
            .docker
            .create_exec(
                &self.container,
                CreateExecOptions {
                    cmd: Some(command.to_vec()),
                    // The output is not read, only the status, but Docker
                    // needs somewhere to put it.
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    tty: Some(false),
                    ..Default::default()
                },
            )
            .await;

        let Ok(created) = created else {
            return false;
        };

        // The exec has to be driven to completion before its status is
        // meaningful; starting it and not reading leaves it pending.
        if let Ok(StartExecResults::Attached { mut output, .. }) =
            self.docker.start_exec(&created.id, None).await
        {
            while output.next().await.is_some() {}
        }

        self.docker
            .inspect_exec(&created.id)
            .await
            .ok()
            .and_then(|inspected| inspected.exit_code)
            .is_some_and(|code| code == 0)
    }
}

/// One container's terminal, for as long as someone is attached to it.
struct DockerTerminal {
    docker: Docker,
    container: String,
    /// Only for what a failure says. The id would mean nothing to anyone.
    service: String,
}

#[async_trait]
impl Resize for DockerTerminal {
    async fn resize(&self, window: kobune_api::Window) -> Result<()> {
        self.docker
            .resize_container_tty(
                &self.container,
                ResizeContainerTtyOptions {
                    width: window.cols,
                    height: window.rows,
                },
            )
            .await
            .map_err(|e| RuntimeError::failed(format!("resizing {}'s terminal", self.service), e))
    }
}

/// How many lines of build output a failure carries with it.
///
/// Enough to see the failing command and what it printed, without turning
/// an error into a wall of text.
const BUILD_CONTEXT_LINES: usize = 12;

/// Where a Dockerfile from outside the build context is placed in the tar.
///
/// Prefixed so it cannot collide with a real file in the context.
const DOCKERFILE_ENTRY: &str = ".kobune-dockerfile";

/// How many seconds a stop waits before it escalates to SIGKILL.
const STOP_TIMEOUT_SECS: i64 = 10;

/// Exit codes that mean "asked to stop" rather than "fell over".
///
/// `docker stop` sends SIGTERM and then SIGKILL, and a process that lets
/// either through exits `128 + signal`. Reading those as failures would
/// paint every `kobune down` red.
///
/// 137 is therefore forgiven, which also forgives an OOM kill. Telling them
/// apart needs `oom_killed`, and that only comes from inspecting each
/// container — a round trip per service on a path that lists them all.
const CLEAN_EXITS: [i64; 3] = [0, 143, 137];

/// The exit code of a container that fell over, if that is what happened.
///
/// **A container that died is not the same as one that was stopped.**
/// Reporting both as `stopped` leaves a start-up script that failed looking
/// like a service nobody started, and nothing but the logs to tell them
/// apart. `None` covers all three ways that is not what happened: no status
/// line, one that cannot be read, and a clean exit.
fn crash_code(status: Option<&str>) -> Option<i64> {
    exit_code_from(status?).filter(|code| !CLEAN_EXITS.contains(code))
}

/// Takes `127` out of `Exited (127) 3 seconds ago`.
///
/// The exit code is not a field of its own on the list response, and
/// inspecting every container to read one would cost a round trip each.
fn exit_code_from(status: &str) -> Option<i64> {
    let (_, rest) = status.split_once('(')?;
    let (code, _) = rest.split_once(')')?;
    code.trim().parse().ok()
}

pub struct DockerRuntime {
    docker: Docker,
    /// The services Kobune itself stopped. See [`DockerRuntime::stop`].
    asked_to_stop: Mutex<HashSet<ServiceKey>>,
    /// One lock per network name, held across
    /// [`DockerRuntime::ensure_network`]. See there for why.
    network_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// What each container with a terminal has made of it, by container
    /// id. Filled by [`DockerRuntime::watch_terminal`] and emptied by the
    /// same watcher when the container's output ends.
    ///
    /// Shared rather than borrowed because the watcher outlives the call
    /// that started it: it runs for as long as the container does.
    terminals: Arc<Mutex<HashMap<String, Arc<Mutex<Modes>>>>>,
}

impl DockerRuntime {
    /// Works out where to connect from the environment and the default
    /// socket.
    ///
    /// Nothing is sent yet, so this succeeds even with Docker down. Actual
    /// reachability is [`Runtime::probe`]'s business.
    pub fn connect() -> Result<Self> {
        let docker =
            Docker::connect_with_local_defaults().map_err(|err| RuntimeError::Unavailable {
                runtime: RUNTIME_ID.to_string(),
                message: err.to_string(),
            })?;

        Ok(Self::with_client(docker))
    }

    pub fn with_client(docker: Docker) -> Self {
        Self {
            docker,
            asked_to_stop: Mutex::new(HashSet::new()),
            network_locks: Mutex::new(HashMap::new()),
            terminals: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The services this runtime stopped and has not started since.
    fn asked_to_stop(&self) -> HashSet<ServiceKey> {
        self.stopped_set().clone()
    }

    /// The set itself.
    ///
    /// **Poisoning is not an error here.** A wave takes this from several
    /// tasks at once, so one panic while it is held would otherwise turn
    /// every later start and stop into a panic inside a request handler.
    /// What it holds is advisory — which services Kobune stopped, used to
    /// tell a clean stop from a crash — so carrying on with whatever is in
    /// it beats taking the daemon down over it.
    fn stopped_set(&self) -> std::sync::MutexGuard<'_, HashSet<ServiceKey>> {
        self.asked_to_stop
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The lock that guards creating one network.
    ///
    /// Made on first use and kept, so two callers naming the same network
    /// get the same lock. There is one per workspace, so the map does not
    /// grow with anything a person does twice.
    fn network_lock(&self, name: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.network_locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(name.to_string())
            .or_default()
            .clone()
    }

    fn unavailable(err: impl std::fmt::Display) -> RuntimeError {
        RuntimeError::Unavailable {
            runtime: RUNTIME_ID.to_string(),
            message: err.to_string(),
        }
    }

    /// Creates the network if it is not there.
    ///
    /// **One caller at a time.** This looks and then creates, and services
    /// starting side by side share a network — so both could look before
    /// either created, and both would then create. Docker does not refuse
    /// a name it already has: it makes a second network with the same name
    /// and a different id, and the two services end up on networks that
    /// cannot reach each other.
    ///
    /// In practice `prepare` runs first on both paths in and creates the
    /// networks there, so what the concurrent callers reach here is the
    /// list and not the create. That is what the lock costs — one listing
    /// at a time — and it is not what the lock is for: a guarantee that
    /// holds only because of what some other function happened to do first
    /// is not one to leave a data race behind.
    ///
    /// **The lock is per network, not one for the runtime.** Two networks
    /// cannot collide with each other, and one `DockerRuntime` serves
    /// every project on the machine — so a single lock would put an
    /// unrelated workspace's listing in front of a wake that has a request
    /// waiting on it.
    async fn ensure_network(&self, key: &WorkspaceKey) -> Result<String> {
        let name = names::network(key);
        let lock = self.network_lock(&name);
        let _guard = lock.lock().await;

        let mut filters = HashMap::new();
        filters.insert("name".to_string(), vec![name.clone()]);

        let existing = self
            .docker
            .list_networks(Some(ListNetworksOptions { filters }))
            .await
            .map_err(|e| RuntimeError::failed("listing networks", e))?;

        // The filter is a prefix match, so only an exact name counts.
        if existing
            .iter()
            .any(|n| n.name.as_deref() == Some(name.as_str()))
        {
            return Ok(name);
        }

        let mut network_labels = HashMap::new();
        network_labels.insert(
            labels::MANAGED.to_string(),
            labels::MANAGED_VALUE.to_string(),
        );
        network_labels.insert(labels::PROJECT.to_string(), key.project.clone());
        network_labels.insert(labels::WORKSPACE.to_string(), key.workspace.clone());

        self.docker
            .create_network(CreateNetworkOptions {
                name: name.clone(),
                driver: "bridge".to_string(),
                labels: network_labels,
                ..Default::default()
            })
            .await
            .map_err(|e| RuntimeError::failed("creating the network", e))?;

        Ok(name)
    }

    /// Builds the image unless that exact one is already here.
    ///
    /// The tag carries a fingerprint of the inputs, so an existing tag means
    /// an image built from exactly this Dockerfile and these args. Skipping
    /// matters most for scale-to-zero: waking a stopped service goes through
    /// `prepare`, and a rebuild there would put a Docker build in the path of
    /// an incoming request.
    async fn ensure_built(
        &self,
        build: &BuildSpec,
        rebuild: bool,
        events: &EventSink,
    ) -> Result<()> {
        let label = format!("building {}", build.tag);

        if !rebuild && self.docker.inspect_image(&build.tag).await.is_ok() {
            events.step_skipped("build", label, "already built");
            return Ok(());
        }

        events.step_started("build", &label);

        let pack = || -> std::io::Result<(Vec<u8>, String)> {
            let mut context = pack_context(&build.context)?;

            // Docker names the Dockerfile by its path inside the tar. One
            // in the context is named where it sits; one from elsewhere in
            // the worktree is added under a reserved name.
            match build.dockerfile.strip_prefix(&build.context) {
                Ok(relative) => Ok((context, relative.to_string_lossy().to_string())),
                Err(_) => {
                    let packed = append_dockerfile(&mut context, &build.dockerfile)?;
                    Ok((packed, DOCKERFILE_ENTRY.to_string()))
                }
            }
        };

        let (context, dockerfile) = pack().map_err(|err| {
            events.step_failed("build", &label, err.to_string());
            RuntimeError::failed(format!("packing the build context for {}", build.tag), err)
        })?;

        let options = BuildImageOptions {
            dockerfile,
            t: build.tag.clone(),
            buildargs: build
                .args
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            // Without this, an intermediate container is left behind for
            // every failed build.
            rm: true,
            forcerm: true,
            ..Default::default()
        };

        let mut stream = self.docker.build_image(options, None, Some(context.into()));

        // The last few lines of output, kept so a failure can say what the
        // build was doing. Docker's own error is often just "exit code 3",
        // and the command that produced it is the part worth reading.
        let mut recent: VecDeque<String> = VecDeque::with_capacity(BUILD_CONTEXT_LINES);

        while let Some(item) = stream.next().await {
            let failure = match item {
                Ok(info) => {
                    // Docker reports progress as the build output itself,
                    // line by line, which is what someone watching a build
                    // wants to see.
                    if let Some(line) = info.stream {
                        let line = line.trim_end();
                        if !line.is_empty() {
                            if recent.len() == BUILD_CONTEXT_LINES {
                                recent.pop_front();
                            }
                            recent.push_back(line.to_string());
                            events.step_progress("build", &label, line);
                        }
                    }

                    // A failing RUN can come back in-band rather than as a
                    // stream error, so both paths have to be handled.
                    info.error
                }
                // bollard folds an in-band failure into this, but its
                // Display drops the message, leaving "Docker stream error"
                // and nothing else. Dig the real one out.
                Err(bollard::errors::Error::DockerStreamError { error }) => Some(error),
                Err(err) => Some(err.to_string()),
            };

            if let Some(error) = failure {
                let message = with_recent_output(error.trim(), &recent);
                events.step_failed("build", &label, message.clone());
                return Err(RuntimeError::failed(
                    format!("building {}", build.tag),
                    message,
                ));
            }
        }

        events.step_done("build", &label);
        Ok(())
    }

    /// Pulls the image unless it is already local.
    async fn ensure_image(&self, image: &str, events: &EventSink) -> Result<()> {
        if self.docker.inspect_image(image).await.is_ok() {
            events.step_skipped("pull", format!("image {image}"), "already present");
            return Ok(());
        }

        events.step_started("pull", format!("pulling image {image}"));

        let mut stream = self.docker.create_image(
            Some(CreateImageOptions {
                from_image: image,
                ..Default::default()
            }),
            None,
            None,
        );

        while let Some(item) = stream.next().await {
            match item {
                Ok(info) => {
                    if let Some(status) = info.status {
                        events.step_progress("pull", format!("pulling image {image}"), status);
                    }
                }
                Err(err) => {
                    events.step_failed("pull", format!("pulling image {image}"), err.to_string());
                    return Err(RuntimeError::ImageUnavailable {
                        image: image.to_string(),
                        message: err.to_string(),
                    });
                }
            }
        }

        events.step_done("pull", format!("pulling image {image}"));
        Ok(())
    }

    /// Makes sure a named volume exists.
    async fn ensure_volume(
        &self,
        key: &WorkspaceKey,
        name: &str,
        scope: crate::spec::VolumeScope,
    ) -> Result<String> {
        let full = names::volume(key, name, scope);

        if self.docker.inspect_volume(&full).await.is_ok() {
            return Ok(full);
        }

        let mut volume_labels = HashMap::new();
        volume_labels.insert(
            labels::MANAGED.to_string(),
            labels::MANAGED_VALUE.to_string(),
        );
        volume_labels.insert(labels::PROJECT.to_string(), key.project.clone());

        // **Only a workspace volume carries a workspace.** That label is
        // what makes it findable when the worktree goes; a project volume
        // outlives every worktree, so labelling it with whichever one
        // happened to create it would be a lie.
        if scope == crate::spec::VolumeScope::Workspace {
            volume_labels.insert(labels::WORKSPACE.to_string(), key.workspace.clone());
        }

        self.docker
            .create_volume(bollard::volume::CreateVolumeOptions {
                name: full.clone(),
                labels: volume_labels,
                ..Default::default()
            })
            .await
            .map_err(|e| RuntimeError::failed("creating the volume", e))?;

        Ok(full)
    }

    /// Starts a throwaway, pumps its output, and waits for its exit code.
    ///
    /// Attached before it is started, so nothing printed in the first
    /// moments is lost — which for a start-up script that fails at once is
    /// the whole output.
    async fn run_throwaway(
        &self,
        id: &str,
        service: &str,
        events: &EventSink,
    ) -> Result<ExecOutcome> {
        // **Claimed before the container runs.** `auto_remove` takes it
        // away the moment it stops, so a wait registered afterwards finds
        // nothing left to report.
        //
        // `next-exit` rather than the default `not-running`: the container
        // has been created and not started, which already counts as not
        // running. That wait returns 0 straight away, and a command that
        // then failed is reported as having succeeded — measured, after it
        // turned `exit 42` into an exit code of 0.
        let waiting = {
            let docker = self.docker.clone();
            let id = id.to_string();
            tokio::spawn(async move {
                docker
                    .wait_container(
                        &id,
                        Some(bollard::container::WaitContainerOptions {
                            condition: "next-exit",
                        }),
                    )
                    .next()
                    .await
            })
        };

        let attached = self
            .docker
            .attach_container(
                id,
                Some(AttachContainerOptions::<String> {
                    stream: Some(true),
                    stdout: Some(true),
                    stderr: Some(true),
                    logs: Some(true),
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| RuntimeError::failed("attaching to the throwaway container", e))?;

        self.docker
            .start_container(id, None::<StartContainerOptions<String>>)
            .await
            .map_err(|e| RuntimeError::failed("starting the throwaway container", e))?;

        let mut output = attached.output;
        while let Some(chunk) = output.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(err) => {
                    // Truncating silently would look like a command that
                    // simply printed less than it did.
                    events.warn(format!("output from the throwaway was cut short: {err}"));
                    break;
                }
            };

            let (stream_kind, bytes) = match chunk {
                LogOutput::StdErr { message } => (OutputStream::Stderr, message),
                LogOutput::StdOut { message }
                | LogOutput::Console { message }
                | LogOutput::StdIn { message } => (OutputStream::Stdout, message),
            };

            for line in String::from_utf8_lossy(&bytes).lines() {
                events.output(Some(service.to_string()), stream_kind, line);
            }
        }

        // `wait` reports a non-zero exit as an error carrying the code, so
        // the code is read from either arm rather than only the happy one.
        //
        // **Nothing at all is a failure, not a zero.** The stream ending
        // without its one response means the connection to Docker broke,
        // and reporting success would tell an agent judging a test by its
        // exit status that the test passed.
        let exit_code = match waiting.await {
            Ok(Some(Ok(response))) => response.status_code,
            Ok(Some(Err(bollard::errors::Error::DockerContainerWaitError { code, .. }))) => code,
            Ok(Some(Err(err))) => {
                return Err(RuntimeError::failed("waiting for the throwaway", err));
            }
            Ok(None) => {
                return Err(RuntimeError::failed(
                    "waiting for the throwaway",
                    "Docker closed the connection without reporting an exit code",
                ));
            }
            Err(err) => return Err(RuntimeError::failed("waiting for the throwaway", err)),
        };

        Ok(ExecOutcome {
            exit_code: exit_code as i32,
        })
    }

    /// Removes the storage that belonged to this worktree and nothing else.
    ///
    /// **Workspace-scoped only.** A project volume is shared and outlives
    /// any one worktree; a workspace one is storage for a worktree that is
    /// being destroyed, so leaving it behind means an unreachable copy of
    /// `node_modules` per branch, for ever.
    ///
    /// Failing here does not fail the removal. The worktree and its
    /// containers are already gone by this point, and refusing to finish
    /// over reclaimable disk would leave a half-removed workspace.
    async fn remove_workspace_volumes(&self, key: &WorkspaceKey, events: &EventSink) {
        let mut filters = HashMap::new();
        filters.insert(
            "label".to_string(),
            vec![
                format!("{}={}", labels::MANAGED, labels::MANAGED_VALUE),
                format!("{}={}", labels::PROJECT, key.project),
                format!("{}={}", labels::WORKSPACE, key.workspace),
            ],
        );

        let listed = match self
            .docker
            .list_volumes(Some(ListVolumesOptions { filters }))
            .await
        {
            Ok(listed) => listed,
            Err(err) => {
                events.debug(format!("cannot list this workspace's volumes: {err}"));
                return;
            }
        };

        for volume in listed.volumes.unwrap_or_default() {
            match self.docker.remove_volume(&volume.name, None).await {
                Ok(()) => events.debug(format!("removed volume {}", volume.name)),
                Err(err) => {
                    events.debug(format!("volume {} was not removed: {err}", volume.name));
                }
            }
        }
    }

    /// The container's summary, if there is a container.
    async fn find_container(&self, key: &ServiceKey) -> Result<Option<ContainerSummary>> {
        let mut filters = HashMap::new();
        filters.insert(
            "label".to_string(),
            vec![
                format!("{}={}", labels::PROJECT, key.workspace.project),
                format!("{}={}", labels::WORKSPACE, key.workspace.workspace),
                format!("{}={}", labels::SERVICE, key.service),
            ],
        );

        let containers = self
            .docker
            .list_containers(Some(ListContainersOptions {
                all: true,
                filters,
                ..Default::default()
            }))
            .await
            .map_err(Self::unavailable)?;

        Ok(containers.into_iter().next())
    }

    /// What the program has made of the terminal it was given.
    ///
    /// **Watched on its way past, not read back afterwards.** The obvious
    /// reading — a terminal container's log *is* what the program wrote to
    /// it, escape sequences and all, so scan it — cannot be made to work,
    /// and the reason is on Docker's side of the pty rather than in any
    /// client: its log holds back everything after the last newline until
    /// the process ends. A program that draws by moving the cursor has no
    /// reason ever to end a line, so the `ESC[?1049h` it announced itself
    /// with sits in that buffer for exactly as long as it is running —
    /// which is exactly whenever anyone would want it.
    ///
    /// Measured rather than deduced: one container shows `starting` alone
    /// while it lives and the announcement as well once it exits, and
    /// `docker logs --follow` waits alongside it.
    ///
    /// So this is [`crate::terminal::Terminal`]'s arrangement, one backend
    /// along: read every chunk as it goes by and keep only what it changed.
    ///
    /// **Empty is a fine answer.** It costs the mouse and the alternate
    /// screen, which is where every attachment stood before any of this
    /// existed. A daemon that restarted since the container started has
    /// exactly that, and says so in the same breath Apple Container's
    /// backend does.
    fn terminal_modes(&self, id: &str) -> Modes {
        let watched = self.terminals().get(id).cloned();

        watched
            .and_then(|modes| modes.lock().ok().map(|modes| modes.clone()))
            .unwrap_or_default()
    }

    /// The terminals being watched.
    ///
    /// **Poisoning is not an error here**, for the reason it is not one in
    /// [`stopped_set`](Self::stopped_set): what this holds is a preamble,
    /// and going without one is a worse terminal rather than a failed
    /// request.
    fn terminals(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Mutex<Modes>>>> {
        self.terminals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Starts watching a container's terminal, before it has run anything.
    ///
    /// **Attached before the container is started**, the way
    /// [`run_throwaway`](Self::run_throwaway) attaches before starting
    /// one. A program announces itself in its first bytes, and nothing
    /// replays them — an attachment a moment late has missed them for
    /// good, which is the whole reason any of this exists.
    ///
    /// Nothing is typed through this one. A client that attaches later
    /// opens its own, and Docker gives each attachment the same output.
    ///
    /// **Never a failure.** A terminal that cannot be watched leaves the
    /// container with no preamble, which is where they all were before.
    async fn watch_terminal(&self, id: &str) {
        let attached = self
            .docker
            .attach_container(
                id,
                Some(AttachContainerOptions::<String> {
                    stream: Some(true),
                    stdout: Some(true),
                    stderr: Some(true),
                    logs: Some(false),
                    ..Default::default()
                }),
            )
            .await;

        let mut output = match attached {
            Ok(attached) => attached.output,
            Err(err) => {
                tracing::debug!("cannot watch {id}'s terminal: {err}");
                return;
            }
        };

        let modes = Arc::new(Mutex::new(Modes::new()));
        self.terminals().insert(id.to_string(), modes.clone());

        // Held rather than borrowed: this outlives the start that began
        // it, and ends when the container's output does.
        let terminals = self.terminals.clone();
        let id = id.to_string();

        tokio::spawn(async move {
            while let Some(chunk) = output.next().await {
                let Ok(chunk) = chunk else { break };

                // A lock that cannot be taken costs the replay and nothing
                // else. Nobody is waiting on this: the bytes are Docker's
                // to deliver to whoever attaches, and this only reads them.
                if let Ok(mut modes) = modes.lock() {
                    modes.watch(&chunk.into_bytes());
                }
            }

            // The container's output ended, so the container has. Its id
            // is never handed out again — `start` creates another rather
            // than restarting this one — so leaving the entry would be a
            // map that only grows.
            terminals
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&id);
        });
    }

    /// Creates the container, replacing any existing one.
    async fn create_container(
        &self,
        spec: &ServiceSpec,
        network: &str,
        throwaway: Option<&Throwaway<'_>>,
    ) -> Result<String> {
        let name = match throwaway {
            Some(one_off) => one_off.name.clone(),
            None => names::container(&spec.key),
        };

        // **A throwaway publishes nothing.** The real container may be
        // holding the port, and a debugging shell has no business taking
        // it from whatever is serving.
        let port = if throwaway.is_some() { None } else { spec.port };

        // **And never gets a terminal.** `run_throwaway` reads stdout and
        // stderr apart to report them apart, and a terminal is one stream.
        // `kobune exec` is also how an agent runs a command, and an agent
        // has no use for a program redrawing itself.
        let terminal = throwaway.is_none() && spec.tty;

        // **A throwaway carries no `SERVICE` label.** That is the one
        // `summary_to_status` needs, so without it a throwaway cannot turn
        // up in `kobune status`, in the routing table, or in what `down`
        // stops. It keeps the others, so one left behind by a daemon that
        // died mid-command is still something `rm` and `purge` can find.
        let mut container_labels = HashMap::new();
        container_labels.insert(
            labels::MANAGED.to_string(),
            labels::MANAGED_VALUE.to_string(),
        );
        container_labels.insert(
            labels::PROJECT.to_string(),
            spec.key.workspace.project.clone(),
        );
        container_labels.insert(
            labels::WORKSPACE.to_string(),
            spec.attached_to.workspace.clone(),
        );

        if throwaway.is_some() {
            container_labels.insert(labels::THROWAWAY.to_string(), "1".to_string());
        }

        if throwaway.is_none() {
            container_labels.insert(
                labels::WORKSPACE.to_string(),
                spec.key.workspace.workspace.clone(),
            );
            container_labels.insert(labels::SERVICE.to_string(), spec.key.service.clone());
            container_labels.insert(
                labels::SCOPE.to_string(),
                match spec.scope {
                    ServiceScope::Workspace => "workspace".to_string(),
                    ServiceScope::Project => "project".to_string(),
                },
            );
            if let Some(port) = port {
                container_labels.insert(labels::PORT.to_string(), port.to_string());
            }
            if terminal {
                container_labels.insert(labels::TTY.to_string(), labels::MANAGED_VALUE.to_string());
            }
        }

        // An empty host port lets Docker pick a free one. Bound to
        // 127.0.0.1 so it never reaches the LAN.
        let mut port_bindings = HashMap::new();
        let mut exposed_ports = HashMap::new();
        if let Some(port) = port {
            let key = format!("{port}/tcp");
            port_bindings.insert(
                key.clone(),
                Some(vec![PortBinding {
                    host_ip: Some("127.0.0.1".to_string()),
                    host_port: Some(String::new()),
                }]),
            );
            exposed_ports.insert(key, HashMap::new());
        }

        let mut mounts = Vec::new();
        if let Some(SourceMount { host, target }) = &spec.source_mount {
            mounts.push(Mount {
                typ: Some(MountTypeEnum::BIND),
                source: Some(host.to_string_lossy().to_string()),
                target: Some(target.clone()),
                ..Default::default()
            });
        }

        for volume in &spec.volumes {
            match volume {
                VolumeMount::Named {
                    name,
                    target,
                    read_only,
                    scope,
                } => {
                    let full = self
                        .ensure_volume(&spec.key.workspace, name, *scope)
                        .await?;
                    mounts.push(Mount {
                        typ: Some(MountTypeEnum::VOLUME),
                        source: Some(full),
                        target: Some(target.clone()),
                        read_only: Some(*read_only),
                        ..Default::default()
                    });
                }
                VolumeMount::Bind {
                    source,
                    target,
                    read_only,
                } => {
                    mounts.push(Mount {
                        typ: Some(MountTypeEnum::BIND),
                        source: Some(source.to_string_lossy().to_string()),
                        target: Some(target.clone()),
                        read_only: Some(*read_only),
                        ..Default::default()
                    });
                }
            }
        }

        let extra_hosts = extra_hosts(spec);

        // Make the service name resolvable, so `api` can reach `db:5432`.
        //
        // A throwaway joins the network — it needs to reach the others —
        // but takes no alias: two containers answering to `api` would send
        // half of `db`'s traffic to a debugging shell.
        let mut endpoints = HashMap::new();
        endpoints.insert(
            network.to_string(),
            EndpointSettings {
                aliases: throwaway.is_none().then(|| vec![spec.key.service.clone()]),
                ..Default::default()
            },
        );

        let config = Config {
            image: Some(spec.image.clone()),
            // Both halves, or neither. A terminal with no stdin is one the
            // program can draw on but nobody can answer, which is the half
            // of interactive that looks like a hang.
            tty: Some(terminal),
            open_stdin: Some(terminal),
            // **Off, deliberately.** With it on, Docker closes the
            // container's stdin as soon as the first attachment leaves,
            // and the next `kobune logs` finds a terminal that takes no
            // keys — for the rest of the container's life.
            stdin_once: Some(false),
            cmd: match throwaway {
                Some(one_off) => Some(one_off.command.to_vec()),
                None => spec.command.clone(),
            },
            working_dir: Some(
                throwaway
                    .and_then(|one_off| one_off.workdir.map(str::to_string))
                    .unwrap_or_else(|| spec.workdir.clone()),
            ),
            env: Some(spec.env_pairs()),
            labels: Some(container_labels),
            exposed_ports: if exposed_ports.is_empty() {
                None
            } else {
                Some(exposed_ports)
            },
            host_config: Some(HostConfig {
                // **Docker reaps a throwaway even if this daemon does
                // not.** A cancelled request drops the future before the
                // explicit removal can run, and the container would
                // otherwise keep running with nobody left to notice. Also
                // takes the image's anonymous volumes with it, which a
                // per-invocation name would accumulate one of each time.
                auto_remove: throwaway.is_some().then_some(true),
                mounts: if mounts.is_empty() {
                    None
                } else {
                    Some(mounts)
                },
                port_bindings: if port_bindings.is_empty() {
                    None
                } else {
                    Some(port_bindings)
                },
                extra_hosts: if extra_hosts.is_empty() {
                    None
                } else {
                    Some(extra_hosts)
                },
                ..Default::default()
            }),
            networking_config: Some(bollard::container::NetworkingConfig {
                endpoints_config: endpoints,
            }),
            ..Default::default()
        };

        let created = self
            .docker
            .create_container(
                Some(CreateContainerOptions {
                    name: name.clone(),
                    platform: None,
                }),
                config,
            )
            .await
            .map_err(|e| RuntimeError::failed(format!("creating container {name}"), e))?;

        Ok(created.id)
    }

    /// The host-side address a running container listens on.
    async fn resolve_endpoint(
        &self,
        container_id: &str,
        port: Option<u16>,
    ) -> Result<Option<SocketAddr>> {
        let Some(port) = port else {
            return Ok(None);
        };

        let details = self
            .docker
            .inspect_container(container_id, None)
            .await
            .map_err(|e| RuntimeError::failed("inspecting the container", e))?;

        let bindings = details
            .network_settings
            .and_then(|settings| settings.ports)
            .and_then(|ports| ports.get(&format!("{port}/tcp")).cloned())
            .flatten();

        let host_port = bindings
            .and_then(|list| list.into_iter().next())
            .and_then(|binding| binding.host_port)
            .and_then(|value| value.parse::<u16>().ok());

        Ok(host_port.map(|p| SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), p)))
    }

    fn summary_to_status(
        summary: &ContainerSummary,
        asked_to_stop: &HashSet<ServiceKey>,
    ) -> Option<ServiceStatus> {
        let labels_map = summary.labels.as_ref()?;

        let project = labels_map.get(labels::PROJECT)?.clone();
        let workspace = labels_map.get(labels::WORKSPACE)?.clone();
        let service = labels_map.get(labels::SERVICE)?.clone();

        let stopped_by_us =
            asked_to_stop.contains(&WorkspaceKey::new(&project, &workspace).service(&service));

        let scope = match labels_map.get(labels::SCOPE).map(String::as_str) {
            Some("project") => ServiceScope::Project,
            _ => ServiceScope::Workspace,
        };

        let port = labels_map
            .get(labels::PORT)
            .and_then(|value| value.parse::<u16>().ok());

        // The `docker ps` state is a string.
        let state = match summary.state.as_deref() {
            Some("running") => ServiceState::Ready,
            Some("created" | "restarting") => ServiceState::Starting,
            // A stop Kobune asked for is a stop whatever the process made
            // of the signal. `turbo` and `next` catch SIGTERM and exit 1
            // themselves, which is indistinguishable from a crash by the
            // exit code alone — and leaves an idle-stopped service sitting
            // in `failed` until someone reads the logs to find out that
            // nothing is wrong.
            Some("exited") if stopped_by_us => ServiceState::Stopped,
            Some("exited") => match crash_code(summary.status.as_deref()) {
                Some(code) => ServiceState::failed(format!(
                    "the container exited with code {code}. \
                     `kobune logs {service}` has the output"
                )),
                None => ServiceState::Stopped,
            },
            Some("dead") => ServiceState::failed(format!(
                "the container is dead. `kobune logs {service}` has whatever it \
                 managed to write"
            )),
            Some("paused" | "removing") => ServiceState::Stopped,
            _ => ServiceState::Unknown,
        };

        // Take the host address from the published port.
        let endpoint = summary
            .ports
            .as_ref()
            .and_then(|ports| {
                ports.iter().find(|p| match (p.private_port, port) {
                    (private, Some(expected)) => private == expected,
                    (_, None) => false,
                })
            })
            .and_then(|p| p.public_port)
            .map(|p| SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), p));

        Some(ServiceStatus {
            key: WorkspaceKey::new(project, workspace).service(service),
            state,
            container_id: summary.id.clone(),
            image: summary.image.clone(),
            endpoint,
            port,
            scope,
        })
    }
}

#[async_trait]
impl Runtime for DockerRuntime {
    fn id(&self) -> &'static str {
        RUNTIME_ID
    }

    async fn probe(&self) -> Result<RuntimeInfo> {
        let version = self.docker.version().await.map_err(Self::unavailable)?;

        Ok(RuntimeInfo {
            id: RUNTIME_ID.to_string(),
            version: version.version.unwrap_or_else(|| "unknown".to_string()),
            supports_custom_networks: true,
        })
    }

    async fn prepare(&self, spec: &WorkspaceSpec, rebuild: bool, events: &EventSink) -> Result<()> {
        events.step_started("network", "preparing the network");
        self.ensure_network(&spec.key).await?;

        // With a shared service around, prepare the shared network too.
        if spec
            .services
            .iter()
            .any(|s| s.scope == ServiceScope::Project)
        {
            self.ensure_network(&WorkspaceKey::shared(&spec.key.project))
                .await?;
        }
        events.step_done("network", "preparing the network");

        // **Pulls overlap; builds do not.**
        //
        // A pull is a download. Three of them at once finish in about the
        // time the slowest takes rather than the sum, the registry is a
        // different machine, and the daemon already de-duplicates the
        // layers two images share.
        //
        // A build is the opposite: it saturates the cores it is given,
        // BuildKit already runs the stages inside one build in parallel,
        // and two builds interleave their output onto a display holding a
        // single line. Running them together would move the same work
        // around rather than remove any of it, and would make the log
        // unreadable while it did.
        //
        // Each is told which service it belongs to, since several now
        // report at once and a step id has to name one thing.
        let (building, pulling): (Vec<_>, Vec<_>) = spec
            .services
            .iter()
            .partition(|service| service.build.is_some());

        let pulls = pulling.iter().map(|service| {
            let events = events.for_service(service.name());
            async move { self.ensure_image(&service.image, &events).await }
        });

        for pulled in futures::future::join_all(pulls).await {
            pulled?;
        }

        for service in building {
            let build = service.build.as_ref().expect("partitioned on it");
            self.ensure_built(build, rebuild, &events.for_service(service.name()))
                .await?;
        }

        Ok(())
    }

    fn starts_concurrently(&self) -> bool {
        true
    }

    async fn start(&self, spec: &ServiceSpec, events: &EventSink) -> Result<RunningService> {
        let name = names::container(&spec.key);

        // Two services can be starting at once, and a step id has to name
        // the one thing it tracks. Scoped once here rather than at each
        // call below, and inherited by the readiness wait further down.
        let events = &events.for_service(spec.name());

        // Whatever it exits with next is nothing to do with the last stop.
        self.stopped_set().remove(&spec.key);

        // Already running: do nothing. `kobune up` gives the same result
        // however many times it is run.
        if let Some(existing) = self.find_container(&spec.key).await? {
            let id = existing.id.clone().unwrap_or_default();

            // Unless it is running the wrong image. A built image is tagged
            // with a fingerprint of its inputs, so an edited Dockerfile
            // produces a new tag — and leaving the old container up would
            // build the new image and then serve the old one.
            let wrong_image = existing
                .image
                .as_deref()
                .is_some_and(|image| image != spec.image);

            // Or without the terminal it should have. Whether a container
            // has one is fixed when it is created, so a `tty` turned on in
            // `kobune.toml` reaches a running service only this way. Left
            // out, the setting would appear to do nothing until something
            // else happened to recreate the container.
            //
            // Read from the label the listing already carried rather than
            // by inspecting: this is on the path of every start, including
            // every wake from scale-to-zero.
            let has_terminal = existing
                .labels
                .as_ref()
                .and_then(|stamped| stamped.get(labels::TTY))
                .map(String::as_str)
                == Some(labels::MANAGED_VALUE);

            let wrong_terminal = has_terminal != spec.tty;

            if !wrong_image && !wrong_terminal && existing.state.as_deref() == Some("running") {
                events.step_skipped(
                    "start",
                    format!("starting {}", spec.name()),
                    "already running",
                );
                let endpoint = self.resolve_endpoint(&id, spec.port).await?;

                // No state emitted. Nothing was waited on here, so there is
                // nothing to claim: the caller settles readiness against
                // `health` before showing anyone an answer, and asserting
                // `ready` from this path would contradict it.

                return Ok(RunningService {
                    key: spec.key.clone(),
                    container_id: id,
                    endpoint,
                });
            }

            // A stopped container may be carrying a stale configuration,
            // and a running one may be on a superseded image. Either way,
            // recreate. That costs a few seconds; a change silently not
            // taking effect costs more.
            self.docker
                .remove_container(
                    &id,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await
                .map_err(|e| RuntimeError::failed(format!("removing container {name}"), e))?;
        }

        events.step_started("start", format!("starting {}", spec.name()));
        events.service_state(spec.name(), ServiceState::Starting);

        let network = names::network(&spec.attached_to);
        self.ensure_network(&spec.attached_to).await?;

        let id = self.create_container(spec, &network, None).await?;

        // A shared service joins the caller's workspace network as well.
        if spec.scope == ServiceScope::Project {
            let shared = self
                .ensure_network(&WorkspaceKey::shared(&spec.key.workspace.project))
                .await?;

            let _ = self
                .docker
                .connect_network(
                    &shared,
                    ConnectNetworkOptions {
                        container: id.clone(),
                        endpoint_config: EndpointSettings {
                            aliases: Some(vec![spec.key.service.clone()]),
                            ..Default::default()
                        },
                    },
                )
                .await;
        }

        // **Before the container runs a single byte.** What this is here
        // to catch is announced in the program's first ones, and Docker
        // replays nothing — see [`watch_terminal`](Self::watch_terminal).
        if spec.tty {
            self.watch_terminal(&id).await;
        }

        self.docker
            .start_container(&id, None::<StartContainerOptions<String>>)
            .await
            .map_err(|e| {
                events.step_failed("start", format!("starting {}", spec.name()), e.to_string());
                RuntimeError::failed(format!("starting container {name}"), e)
            })?;

        // **A terminal Docker has just created has no size at all**, and
        // gets one only when a client attaches. A program that draws a
        // full-screen interface asks at start-up, long before that, and
        // one that cannot be told falls back to plain scrolling output for
        // the rest of its life. So the size is given now rather than left
        // to whoever attaches later — see [`DEFAULT_WINDOW`].
        if spec.tty
            && let Err(err) = self
                .docker
                .resize_container_tty(
                    &id,
                    ResizeContainerTtyOptions {
                        width: DEFAULT_WINDOW.cols,
                        height: DEFAULT_WINDOW.rows,
                    },
                )
                .await
        {
            events.debug(format!(
                "cannot size {}'s terminal: {err}. A full-screen program \
                 may fall back to plain output",
                spec.name()
            ));
        }

        let endpoint = self.resolve_endpoint(&id, spec.port).await?;

        events.step_done("start", format!("starting {}", spec.name()));

        // A container being up does not mean the app inside is listening.
        // Without this wait, the curl right after `kobune new` fails with
        // connection refused.
        let probe = DockerCommandProbe {
            docker: self.docker.clone(),
            container: id.clone(),
        };

        // **The answer is used, not just waited on.** Running out of time
        // here is not a failure — a dev server's first build can outlast
        // any sensible wait — but it does mean the service is still coming
        // up, and saying `ready` regardless is what made the state useless
        // for deciding whether to wait.
        let ready = await_service(
            spec.name(),
            endpoint,
            spec.health.as_ref(),
            Some(&probe),
            DEFAULT_READINESS_TIMEOUT,
            events,
        )
        .await;

        events.service_state(
            spec.name(),
            if ready {
                ServiceState::Ready
            } else {
                ServiceState::Starting
            },
        );

        Ok(RunningService {
            key: spec.key.clone(),
            container_id: id,
            endpoint,
        })
    }

    async fn stop(&self, key: &ServiceKey, events: &EventSink) -> Result<()> {
        // Scoped like `start`: a step id names the one service it is
        // tracking, whether or not anything overlaps today.
        let events = &events.for_service(&key.service);
        let Some(container) = self.find_container(key).await? else {
            events.step_skipped("stop", format!("stopping {}", key.service), "not running");
            return Ok(());
        };

        let id = container.id.unwrap_or_default();
        if container.state.as_deref() != Some("running") {
            events.step_skipped("stop", format!("stopping {}", key.service), "not running");
            return Ok(());
        }

        events.step_started("stop", format!("stopping {}", key.service));

        self.docker
            .stop_container(
                &id,
                Some(StopContainerOptions {
                    t: STOP_TIMEOUT_SECS,
                }),
            )
            .await
            .map_err(|e| RuntimeError::failed(format!("stopping container {id}"), e))?;

        // Remembered so the exit code it produced is not read as a crash.
        // Only for as long as this runtime lives: after a daemon restart
        // nothing knows who stopped what, and the exit code is all there
        // is to go on again.
        self.stopped_set().insert(key.clone());

        events.step_done("stop", format!("stopping {}", key.service));
        events.service_state(&key.service, ServiceState::Stopped);
        Ok(())
    }

    async fn remove(&self, key: &ServiceKey, events: &EventSink) -> Result<()> {
        let events = &events.for_service(&key.service);
        let Some(container) = self.find_container(key).await? else {
            return Ok(());
        };

        let id = container.id.unwrap_or_default();
        events.step_started("remove", format!("removing {}", key.service));

        self.docker
            .remove_container(
                &id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| RuntimeError::failed(format!("removing container {id}"), e))?;

        // There is no longer a container whose exit code needs explaining.
        self.stopped_set().remove(key);

        events.step_done("remove", format!("removing {}", key.service));
        Ok(())
    }

    async fn destroy_workspace(&self, key: &WorkspaceKey, events: &EventSink) -> Result<()> {
        let mut filters = HashMap::new();
        filters.insert(
            "label".to_string(),
            vec![
                format!("{}={}", labels::PROJECT, key.project),
                format!("{}={}", labels::WORKSPACE, key.workspace),
            ],
        );

        let containers = self
            .docker
            .list_containers(Some(ListContainersOptions {
                all: true,
                filters,
                ..Default::default()
            }))
            .await
            .map_err(Self::unavailable)?;

        events.step_started("destroy", "removing containers");
        for container in containers {
            if let Some(id) = container.id {
                self.docker
                    .remove_container(
                        &id,
                        Some(RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await
                    .map_err(|e| RuntimeError::failed(format!("removing container {id}"), e))?;
            }
        }
        events.step_done("destroy", "removing containers");

        // The network goes away once nobody else is on it. Failing here
        // is not fatal.
        let network = names::network(key);
        if let Err(err) = self.docker.remove_network(&network).await {
            events.debug(format!("network {network} was not removed: {err}"));
        }

        self.remove_workspace_volumes(key, events).await;

        Ok(())
    }

    async fn inspect(&self, key: &ServiceKey) -> Result<ServiceStatus> {
        match self.find_container(key).await? {
            Some(summary) => Ok(Self::summary_to_status(&summary, &self.asked_to_stop())
                .unwrap_or_else(|| ServiceStatus::stopped(key.clone(), ServiceScope::Workspace))),
            None => Ok(ServiceStatus::stopped(key.clone(), ServiceScope::Workspace)),
        }
    }

    async fn logs(
        &self,
        key: &ServiceKey,
        options: LogOptions,
    ) -> Result<BoxStream<'static, LogLine>> {
        let container = self.find_container(key).await?.ok_or_else(|| {
            RuntimeError::failed(
                format!("reading logs for {}", key.service),
                "there is no container",
            )
        })?;

        let id = container.id.unwrap_or_default();
        let stream = self.docker.logs(
            &id,
            Some(LogsOptions::<String> {
                follow: options.follow,
                stdout: true,
                stderr: true,
                tail: options
                    .tail
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "all".to_string()),
                ..Default::default()
            }),
        );

        // Docker can pack several lines into one chunk. Split them.
        let lines = stream.flat_map(|item| {
            let chunk = match item {
                Ok(output) => output,
                Err(err) => {
                    tracing::debug!("log reading finished: {err}");
                    return futures::stream::iter(Vec::new());
                }
            };

            let (stream_kind, bytes) = match chunk {
                LogOutput::StdErr { message } => (OutputStream::Stderr, message),
                LogOutput::StdOut { message }
                | LogOutput::Console { message }
                | LogOutput::StdIn { message } => (OutputStream::Stdout, message),
            };

            // Docker can pack several lines into one chunk; the carriage
            // return a terminal leaves on each is `LogLine::new`'s to take
            // off.
            let text = String::from_utf8_lossy(&bytes);
            let lines: Vec<LogLine> = text
                .lines()
                .map(|line| LogLine::new(stream_kind, line.to_string()))
                .collect();

            futures::stream::iter(lines)
        });

        Ok(Box::pin(lines))
    }

    async fn attach(&self, key: &ServiceKey) -> Result<Attachment> {
        let container = self.find_container(key).await?.ok_or_else(|| {
            RuntimeError::failed(
                format!("attaching to {}", key.service),
                "there is no container. Start it with `kobune up`",
            )
        })?;

        if container.state.as_deref() != Some("running") {
            return Err(RuntimeError::failed(
                format!("attaching to {}", key.service),
                "the container is not running. Start it with `kobune up`",
            ));
        }

        let id = container.id.unwrap_or_default();
        let attached = self
            .docker
            .attach_container(
                &id,
                Some(AttachContainerOptions::<String> {
                    stdin: Some(true),
                    stdout: Some(true),
                    stderr: Some(true),
                    stream: Some(true),
                    // **No output replayed.** A full-screen program's past
                    // output is a record of a screen that no longer
                    // exists; drawing it again produces a mess, and the
                    // program redraws in full anyway. Whoever wants the
                    // history has `kobune logs` without `-f`.
                    //
                    // What the program *said about the terminal* is
                    // replayed, and separately — see the preamble below.
                    logs: Some(false),
                    // Left to Docker's default, and never triggered:
                    // detaching is the client's business, and it holds the
                    // keys before they get this far.
                    detach_keys: None,
                }),
            )
            .await
            .map_err(|e| RuntimeError::failed(format!("attaching to {}", key.service), e))?;

        // Taken from the watcher that has been reading this terminal since
        // before the container started. Nothing is read back here, and
        // nothing can be — see [`terminal_modes`](Self::terminal_modes).
        let preamble = self.terminal_modes(&id).preamble();

        // With a terminal there is one stream, and Docker reports it as
        // `Console`. The rest are matched so that a container that turns
        // out not to have one is passed through rather than silently
        // dropped.
        let output = attached.output.filter_map(|item| async move {
            match item {
                // Which of Docker's streams a chunk came from does not
                // matter here: with a terminal there is only one, and a
                // screen is not something to sort by stream anyway.
                Ok(chunk) => Some(chunk.into_bytes().to_vec()),
                Err(err) => {
                    tracing::debug!("the attachment ended: {err}");
                    None
                }
            }
        });

        Ok(Attachment::opening_with(
            preamble,
            Box::pin(output),
            attached.input,
            // The container this was opened on, kept: a window drag then
            // costs one call each rather than a lookup and a call.
            Sizing::Follows(Box::new(DockerTerminal {
                docker: self.docker.clone(),
                container: id,
                service: key.service.clone(),
            })),
        ))
    }

    async fn exec(
        &self,
        key: &ServiceKey,
        command: &[String],
        options: &ExecOptions,
        events: &EventSink,
    ) -> Result<ExecOutcome> {
        let container = self.find_container(key).await?.ok_or_else(|| {
            RuntimeError::failed(
                format!("running a command in {}", key.service),
                "there is no container. Start it with `kobune up`",
            )
        })?;

        if container.state.as_deref() != Some("running") {
            // Wanting to exec into a container is at its most likely just
            // after one fell over, and "start it with `kobune up`" describes
            // the wrong problem.
            let detail = match crash_code(container.status.as_deref()) {
                Some(code) => format!(
                    "the container exited with code {code}. `kobune logs {}` says why",
                    key.service
                ),
                None => "the container is not running. Start it with `kobune up`".to_string(),
            };

            return Err(RuntimeError::failed(
                format!("running a command in {}", key.service),
                detail,
            ));
        }

        let id = container.id.unwrap_or_default();
        let created = self
            .docker
            .create_exec(
                &id,
                CreateExecOptions {
                    cmd: Some(command.to_vec()),
                    working_dir: options.workdir.clone(),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    // No TTY: hanging on a prompt is the worse outcome.
                    tty: Some(false),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| RuntimeError::failed("creating the exec", e))?;

        let started = self
            .docker
            .start_exec(&created.id, None)
            .await
            .map_err(|e| RuntimeError::failed("starting the exec", e))?;

        if let StartExecResults::Attached { mut output, .. } = started {
            while let Some(chunk) = output.next().await {
                let Ok(chunk) = chunk else { break };

                let (stream_kind, bytes) = match chunk {
                    LogOutput::StdErr { message } => (OutputStream::Stderr, message),
                    LogOutput::StdOut { message }
                    | LogOutput::Console { message }
                    | LogOutput::StdIn { message } => (OutputStream::Stdout, message),
                };

                for line in String::from_utf8_lossy(&bytes).lines() {
                    events.output(Some(key.service.clone()), stream_kind, line);
                }
            }
        }

        let inspected = self
            .docker
            .inspect_exec(&created.id)
            .await
            .map_err(|e| RuntimeError::failed("inspecting the exec", e))?;

        Ok(ExecOutcome {
            exit_code: inspected.exit_code.unwrap_or(-1) as i32,
        })
    }

    async fn exec_fresh(
        &self,
        spec: &ServiceSpec,
        command: &[String],
        options: &ExecOptions,
        events: &EventSink,
    ) -> Result<ExecOutcome> {
        let network = self.ensure_network(&spec.key.workspace).await?;
        let one_off = Throwaway::new(spec, command, options.workdir.as_deref());
        let name = one_off.name.clone();

        let id = self
            .create_container(spec, &network, Some(&one_off))
            .await?;

        // Removed whatever happens next, including the command failing or
        // the caller hanging up. A debugging aid that leaves containers
        // behind stops being one.
        let outcome = self.run_throwaway(&id, &spec.key.service, events).await;

        if let Err(err) = self
            .docker
            .remove_container(
                &id,
                Some(RemoveContainerOptions {
                    force: true,
                    // The image's anonymous volumes go too. A throwaway
                    // gets a new name every run, so one left behind per
                    // invocation would accumulate without bound.
                    v: true,
                    ..Default::default()
                }),
            )
            .await
        {
            // `auto_remove` has usually got there first, so this is
            // expected to fail as often as not.
            events.debug(format!("throwaway container {name} was not removed: {err}"));
        }

        outcome
    }

    async fn list_project(&self, project: &str) -> Result<Vec<ServiceStatus>> {
        let mut filters = HashMap::new();
        filters.insert(
            "label".to_string(),
            vec![format!("{}={}", labels::PROJECT, project)],
        );

        let containers = self
            .docker
            .list_containers(Some(ListContainersOptions {
                all: true,
                filters,
                ..Default::default()
            }))
            .await
            .map_err(Self::unavailable)?;

        let asked_to_stop = self.asked_to_stop();

        Ok(containers
            .iter()
            .filter_map(|summary| Self::summary_to_status(summary, &asked_to_stop))
            .collect())
    }

    async fn managed_volumes(&self) -> Result<Vec<ManagedVolume>> {
        // The managed label alone. Filtering by project as well would only
        // find the projects somebody already knew to ask about, and the
        // ones worth finding here are the ones nothing remembers.
        let mut filters = HashMap::new();
        filters.insert(
            "label".to_string(),
            vec![format!("{}={}", labels::MANAGED, labels::MANAGED_VALUE)],
        );

        let listed = self
            .docker
            .list_volumes(Some(ListVolumesOptions { filters }))
            .await
            .map_err(Self::unavailable)?;

        Ok(listed
            .volumes
            .unwrap_or_default()
            .into_iter()
            .map(|volume| ManagedVolume {
                project: volume
                    .labels
                    .get(labels::PROJECT)
                    .cloned()
                    .unwrap_or_default(),
                id: volume.name,
            })
            .collect())
    }

    async fn remove_managed_volume(&self, volume: &ManagedVolume) -> Result<()> {
        self.docker
            .remove_volume(&volume.id, None)
            .await
            .map_err(|e| RuntimeError::failed(format!("removing volume {}", volume.id), e))
    }
}

/// Puts the tail of the build output alongside the error.
///
/// "The command '/bin/sh -c npm ci' returned a non-zero code: 1" says which
/// command failed and nothing about why. What npm printed is the answer, and
/// it has already gone past as progress.
fn with_recent_output(error: &str, recent: &VecDeque<String>) -> String {
    if recent.is_empty() {
        return error.to_string();
    }

    let output = recent
        .iter()
        .map(|line| format!("  {line}"))
        .collect::<Vec<_>>()
        .join("\n");

    format!("{error}\n\nthe build was doing:\n{output}")
}

/// Tars a build context for the Docker API.
///
/// The API takes the context as a tar stream, so the whole directory is read
/// into memory. That is fine for the contexts this is aimed at — a
/// Dockerfile and a lock file — and a `.dockerignore` keeps a `node_modules`
/// out of it.
fn pack_context(context: &Path) -> std::io::Result<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::new());
    builder.follow_symlinks(false);
    builder.append_dir_all(".", context)?;
    builder.into_inner()
}

/// Adds a Dockerfile that lives outside the context.
///
/// `dockerfile` may point anywhere in the worktree, so one context can build
/// several images. Docker names the Dockerfile by its path inside the tar,
/// so an outside one has to be placed into it.
fn append_dockerfile(context: &mut Vec<u8>, dockerfile: &Path) -> std::io::Result<Vec<u8>> {
    let contents = std::fs::read(dockerfile)?;

    let mut builder = tar::Builder::new(std::mem::take(context));
    let mut header = tar::Header::new_gnu();
    header.set_size(contents.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append_data(&mut header, DOCKERFILE_ENTRY, contents.as_slice())?;

    builder.into_inner()
}

/// The workspace's own URLs, pointed at the host where the proxy is.
///
/// `host-gateway` is Docker's name for "wherever the host is from in
/// here", which Docker Desktop resolves to the address that reaches the
/// host's loopback — the only addresses the proxy holds. Naming an address
/// instead would be naming Docker Desktop's internals, which are not ours
/// to depend on.
///
/// **A throwaway gets them too.** `kobune exec` is where a service is
/// poked at by hand, and a curl that works in the service but not in the
/// shell beside it is the confusing kind of difference.
fn extra_hosts(spec: &ServiceSpec) -> Vec<String> {
    spec.gateway_hosts
        .iter()
        .map(|host| format!("{host}:host-gateway"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::Port;
    use std::collections::HashMap;

    fn summary_with(labels_map: HashMap<String, String>, state: &str) -> ContainerSummary {
        ContainerSummary {
            id: Some("abc123".into()),
            image: Some("node:22".into()),
            state: Some(state.into()),
            labels: Some(labels_map),
            ports: Some(vec![Port {
                private_port: 3000,
                public_port: Some(49312),
                ip: Some("127.0.0.1".into()),
                typ: None,
            }]),
            ..Default::default()
        }
    }

    /// No service has been stopped by us, which is every case but one.
    fn not_stopped() -> HashSet<ServiceKey> {
        HashSet::new()
    }

    fn kobune_labels() -> HashMap<String, String> {
        HashMap::from([
            (labels::MANAGED.to_string(), "1".to_string()),
            (labels::PROJECT.to_string(), "myapp".to_string()),
            (labels::WORKSPACE.to_string(), "feat-1".to_string()),
            (labels::SERVICE.to_string(), "web".to_string()),
            (labels::SCOPE.to_string(), "workspace".to_string()),
            (labels::PORT.to_string(), "3000".to_string()),
        ])
    }

    #[test]
    fn points_the_service_urls_at_the_host() {
        // Without this the URL a container is handed resolves to nothing
        // inside it, and server-to-server calls have to use a different
        // Host than the browser does.
        let mut spec = spec_for_hosts();
        spec.gateway_hosts = vec![
            "web.feat-1.myapp.localhost".into(),
            "api.feat-1.myapp.localhost".into(),
        ];

        assert_eq!(
            extra_hosts(&spec),
            vec![
                "web.feat-1.myapp.localhost:host-gateway".to_string(),
                "api.feat-1.myapp.localhost:host-gateway".to_string(),
            ]
        );
    }

    #[test]
    fn adds_no_hosts_when_there_are_no_urls() {
        // No proxy, no URLs, and so nothing that has to resolve.
        assert!(extra_hosts(&spec_for_hosts()).is_empty());
    }

    fn spec_for_hosts() -> ServiceSpec {
        ServiceSpec {
            key: WorkspaceKey::new("myapp", "feat-1").service("web"),
            attached_to: WorkspaceKey::new("myapp", "feat-1"),
            image: "node:22".into(),
            build: None,
            command: None,
            workdir: "/workspace".into(),
            env: Default::default(),
            tty: false,
            port: Some(3000),
            health: None,
            scope: ServiceScope::Workspace,
            volumes: vec![],
            source_mount: None,
            peers: vec![],
            gateway_hosts: vec![],
        }
    }

    #[test]
    fn reconstructs_state_from_labels() {
        // A daemon restart has nothing but this to recover its state
        // from.
        let status = DockerRuntime::summary_to_status(
            &summary_with(kobune_labels(), "running"),
            &not_stopped(),
        )
        .expect("recovers when the Kobune labels are all there");

        assert_eq!(status.key.workspace.project, "myapp");
        assert_eq!(status.key.workspace.workspace, "feat-1");
        assert_eq!(status.key.service, "web");
        assert_eq!(status.state, ServiceState::Ready);
        assert_eq!(status.port, Some(3000));
        assert_eq!(status.scope, ServiceScope::Workspace);
        assert_eq!(
            status.endpoint,
            Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49312))
        );
    }

    #[test]
    fn ignores_containers_without_kobune_labels() {
        let mut foreign = HashMap::new();
        foreign.insert("com.example.app".to_string(), "other".to_string());

        assert!(
            DockerRuntime::summary_to_status(&summary_with(foreign, "running"), &not_stopped())
                .is_none(),
            "someone else's container must not be picked up"
        );
    }

    #[test]
    fn maps_docker_states() {
        let cases = [
            ("running", ServiceState::Ready),
            ("created", ServiceState::Starting),
            ("restarting", ServiceState::Starting),
            ("paused", ServiceState::Stopped),
        ];

        for (docker_state, expected) in cases {
            let status = DockerRuntime::summary_to_status(
                &summary_with(kobune_labels(), docker_state),
                &not_stopped(),
            )
            .expect("recovers");
            assert_eq!(status.state, expected, "docker state = {docker_state}");
        }
    }

    /// A summary that also carries the `Status` line `docker ps` shows.
    fn exited_with(status: &str) -> ContainerSummary {
        ContainerSummary {
            status: Some(status.into()),
            ..summary_with(kobune_labels(), "exited")
        }
    }

    #[test]
    fn a_container_that_fell_over_is_failed_not_stopped() {
        // The reason SKILL.md promises. Without it a start-up script that
        // died looks exactly like a service nobody started.
        let status = DockerRuntime::summary_to_status(
            &exited_with("Exited (127) 2 seconds ago"),
            &not_stopped(),
        )
        .expect("recovers");

        let ServiceState::Failed { reason } = &status.state else {
            panic!("expected a failure, got {:?}", status.state);
        };
        assert!(reason.contains("127"), "name the exit code: {reason}");
        assert!(
            reason.contains("kobune logs web"),
            "say where to look: {reason}"
        );
    }

    #[test]
    fn a_dead_container_is_failed_too() {
        let status = DockerRuntime::summary_to_status(
            &summary_with(kobune_labels(), "dead"),
            &not_stopped(),
        )
        .expect("recovers");

        let ServiceState::Failed { reason } = &status.state else {
            panic!("expected a failure, got {:?}", status.state);
        };
        assert!(reason.contains("kobune logs web"), "{reason}");
    }

    #[test]
    fn anything_but_a_crash_stays_stopped() {
        // `docker stop` sends SIGTERM, and a shell exits 143 for it, so
        // every `kobune down` would otherwise end in red. An unreadable or
        // absent status line must not be guessed at either.
        for line in [
            "Exited (0) 1 second ago",
            "Exited (143) 1 second ago",
            "Exited (137) 1 second ago",
            "Exited (oops) 1 second ago",
        ] {
            let status = DockerRuntime::summary_to_status(&exited_with(line), &not_stopped())
                .expect("recovers");
            assert_eq!(status.state, ServiceState::Stopped, "{line}");
        }

        let no_status = DockerRuntime::summary_to_status(
            &summary_with(kobune_labels(), "exited"),
            &not_stopped(),
        )
        .expect("ok");
        assert_eq!(no_status.state, ServiceState::Stopped, "no status line");
    }

    #[test]
    fn a_stop_we_asked_for_is_not_a_crash_whatever_the_exit_code() {
        // The idle reaper stops a service, `turbo` catches the SIGTERM and
        // exits 1 of its own accord, and the workspace is left reporting a
        // failure nobody caused.
        let stopped = HashSet::from([WorkspaceKey::new("myapp", "feat-1").service("web")]);

        let status =
            DockerRuntime::summary_to_status(&exited_with("Exited (1) 30 minutes ago"), &stopped)
                .expect("recovers");

        assert_eq!(status.state, ServiceState::Stopped);
    }

    #[test]
    fn a_crash_after_a_stop_of_a_different_service_is_still_a_crash() {
        // The record is per service, so one stopped service must not
        // excuse another one falling over.
        let stopped = HashSet::from([WorkspaceKey::new("myapp", "feat-1").service("api")]);

        let status =
            DockerRuntime::summary_to_status(&exited_with("Exited (1) 2 seconds ago"), &stopped)
                .expect("recovers");

        assert!(
            matches!(status.state, ServiceState::Failed { .. }),
            "got {:?}",
            status.state
        );
    }

    #[test]
    fn a_crash_is_told_from_a_clean_exit() {
        assert_eq!(crash_code(Some("Exited (127) 2 seconds ago")), Some(127));

        for clean in [
            Some("Exited (0) 5 minutes ago"),
            Some("Exited (143) 5 minutes ago"),
            Some("Up 3 hours"),
            Some("Exited (oops) ago"),
            None,
        ] {
            assert_eq!(crash_code(clean), None, "{clean:?}");
        }
    }

    #[test]
    fn recognises_shared_scope() {
        let mut shared = kobune_labels();
        shared.insert(labels::SCOPE.to_string(), "project".to_string());
        shared.insert(labels::WORKSPACE.to_string(), "_shared".to_string());

        let status =
            DockerRuntime::summary_to_status(&summary_with(shared, "running"), &not_stopped())
                .expect("recovers");

        assert_eq!(status.scope, ServiceScope::Project);
        assert!(status.key.workspace.is_shared());
    }

    #[test]
    fn endpoint_is_absent_when_port_not_published() {
        let mut no_port = kobune_labels();
        no_port.remove(labels::PORT);

        let mut summary = summary_with(no_port, "running");
        summary.ports = None;

        let status = DockerRuntime::summary_to_status(&summary, &not_stopped()).expect("recovers");
        assert_eq!(status.port, None);
        assert_eq!(status.endpoint, None);
    }
}
