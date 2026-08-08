//! The Docker backend.
//!
//! Talks to the Docker API directly rather than shelling out to `docker
//! compose`. Building on compose would cap the design at "whatever compose
//! can do", and the other runtimes could not follow.
//!
//! Ports are forwarded to a dynamically assigned port on `127.0.0.1`.
//! Exposing them on `0.0.0.0` would put the development environment in
//! front of everyone else on the LAN.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;

use async_trait::async_trait;
use bollard::Docker;
use bollard::container::{
    Config, CreateContainerOptions, ListContainersOptions, LogOutput, LogsOptions,
    RemoveContainerOptions, StartContainerOptions, StopContainerOptions,
};
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::image::{BuildImageOptions, CreateImageOptions};
use bollard::models::{
    ContainerSummary, EndpointSettings, HostConfig, Mount, MountTypeEnum, PortBinding,
};
use bollard::network::{ConnectNetworkOptions, CreateNetworkOptions, ListNetworksOptions};
use futures::StreamExt;
use futures::stream::BoxStream;
use minato_api::OutputStream;
use minato_core::{ServiceScope, ServiceState};

use crate::error::{Result, RuntimeError};
use crate::event::EventSink;
use crate::health::{DEFAULT_READINESS_TIMEOUT, await_service};
use crate::runtime::{ExecOutcome, LogLine, LogOptions, Runtime, RuntimeInfo, labels, names};
use crate::spec::{
    BuildSpec, RunningService, ServiceKey, ServiceSpec, ServiceStatus, SourceMount, VolumeMount,
    WorkspaceKey, WorkspaceSpec,
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

/// How many lines of build output a failure carries with it.
///
/// Enough to see the failing command and what it printed, without turning
/// an error into a wall of text.
const BUILD_CONTEXT_LINES: usize = 12;

/// Where a Dockerfile from outside the build context is placed in the tar.
///
/// Prefixed so it cannot collide with a real file in the context.
const DOCKERFILE_ENTRY: &str = ".minato-dockerfile";

/// How many seconds a stop waits before it escalates to SIGKILL.
const STOP_TIMEOUT_SECS: i64 = 10;

/// Exit codes that mean "asked to stop" rather than "fell over".
///
/// `docker stop` sends SIGTERM and then SIGKILL, and a process that lets
/// either through exits `128 + signal`. Reading those as failures would
/// paint every `minato down` red.
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

        Ok(Self { docker })
    }

    pub fn with_client(docker: Docker) -> Self {
        Self { docker }
    }

    fn unavailable(err: impl std::fmt::Display) -> RuntimeError {
        RuntimeError::Unavailable {
            runtime: RUNTIME_ID.to_string(),
            message: err.to_string(),
        }
    }

    /// Creates the network if it is not there.
    async fn ensure_network(&self, key: &WorkspaceKey) -> Result<String> {
        let name = names::network(key);

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
    async fn ensure_volume(&self, project: &str, name: &str) -> Result<String> {
        let full = names::volume(project, name);

        if self.docker.inspect_volume(&full).await.is_ok() {
            return Ok(full);
        }

        let mut volume_labels = HashMap::new();
        volume_labels.insert(
            labels::MANAGED.to_string(),
            labels::MANAGED_VALUE.to_string(),
        );
        volume_labels.insert(labels::PROJECT.to_string(), project.to_string());

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

    /// Creates the container, replacing any existing one.
    async fn create_container(&self, spec: &ServiceSpec, network: &str) -> Result<String> {
        let name = names::container(&spec.key);
        let port = spec.port;

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
                } => {
                    let full = self
                        .ensure_volume(&spec.key.workspace.project, name)
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

        // Make the service name resolvable, so `api` can reach
        // `db:5432`.
        let mut endpoints = HashMap::new();
        endpoints.insert(
            network.to_string(),
            EndpointSettings {
                aliases: Some(vec![spec.key.service.clone()]),
                ..Default::default()
            },
        );

        let config = Config {
            image: Some(spec.image.clone()),
            cmd: spec.command.clone(),
            working_dir: Some(spec.workdir.clone()),
            env: Some(spec.env_pairs()),
            labels: Some(container_labels),
            exposed_ports: if exposed_ports.is_empty() {
                None
            } else {
                Some(exposed_ports)
            },
            host_config: Some(HostConfig {
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

    fn summary_to_status(summary: &ContainerSummary) -> Option<ServiceStatus> {
        let labels_map = summary.labels.as_ref()?;

        let project = labels_map.get(labels::PROJECT)?.clone();
        let workspace = labels_map.get(labels::WORKSPACE)?.clone();
        let service = labels_map.get(labels::SERVICE)?.clone();

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
            Some("exited") => match crash_code(summary.status.as_deref()) {
                Some(code) => ServiceState::failed(format!(
                    "the container exited with code {code}. \
                     `minato logs {service}` has the output"
                )),
                None => ServiceState::Stopped,
            },
            Some("dead") => ServiceState::failed(format!(
                "the container is dead. `minato logs {service}` has whatever it \
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

        for service in &spec.services {
            match &service.build {
                Some(build) => self.ensure_built(build, rebuild, events).await?,
                None => self.ensure_image(&service.image, events).await?,
            }
        }

        Ok(())
    }

    async fn start(&self, spec: &ServiceSpec, events: &EventSink) -> Result<RunningService> {
        let name = names::container(&spec.key);

        // Already running: do nothing. `minato up` gives the same result
        // however many times it is run.
        if let Some(existing) = self.find_container(&spec.key).await? {
            let id = existing.id.clone().unwrap_or_default();

            // Unless it is running the wrong image. A built image is tagged
            // with a fingerprint of its inputs, so an edited Dockerfile
            // produces a new tag — and leaving the old container up would
            // build the new image and then serve the old one.
            let stale = existing
                .image
                .as_deref()
                .is_some_and(|image| image != spec.image);

            if !stale && existing.state.as_deref() == Some("running") {
                events.step_skipped(
                    "start",
                    format!("starting {}", spec.name()),
                    "already running",
                );
                let endpoint = self.resolve_endpoint(&id, spec.port).await?;
                events.service_state(spec.name(), ServiceState::Ready);

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

        let id = self.create_container(spec, &network).await?;

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

        self.docker
            .start_container(&id, None::<StartContainerOptions<String>>)
            .await
            .map_err(|e| {
                events.step_failed("start", format!("starting {}", spec.name()), e.to_string());
                RuntimeError::failed(format!("starting container {name}"), e)
            })?;

        let endpoint = self.resolve_endpoint(&id, spec.port).await?;

        events.step_done("start", format!("starting {}", spec.name()));

        // A container being up does not mean the app inside is listening.
        // Without this wait, the curl right after `minato new` fails with
        // connection refused.
        let probe = DockerCommandProbe {
            docker: self.docker.clone(),
            container: id.clone(),
        };

        await_service(
            spec.name(),
            endpoint,
            spec.health.as_ref(),
            Some(&probe),
            DEFAULT_READINESS_TIMEOUT,
            events,
        )
        .await;

        events.service_state(spec.name(), ServiceState::Ready);

        Ok(RunningService {
            key: spec.key.clone(),
            container_id: id,
            endpoint,
        })
    }

    async fn stop(&self, key: &ServiceKey, events: &EventSink) -> Result<()> {
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

        events.step_done("stop", format!("stopping {}", key.service));
        events.service_state(&key.service, ServiceState::Stopped);
        Ok(())
    }

    async fn remove(&self, key: &ServiceKey, events: &EventSink) -> Result<()> {
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

        Ok(())
    }

    async fn inspect(&self, key: &ServiceKey) -> Result<ServiceStatus> {
        match self.find_container(key).await? {
            Some(summary) => Ok(Self::summary_to_status(&summary)
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

            let text = String::from_utf8_lossy(&bytes).to_string();
            let lines: Vec<LogLine> = text
                .lines()
                .map(|line| LogLine {
                    stream: stream_kind,
                    line: line.to_string(),
                })
                .collect();

            futures::stream::iter(lines)
        });

        Ok(Box::pin(lines))
    }

    async fn exec(
        &self,
        key: &ServiceKey,
        command: &[String],
        events: &EventSink,
    ) -> Result<ExecOutcome> {
        let container = self.find_container(key).await?.ok_or_else(|| {
            RuntimeError::failed(
                format!("running a command in {}", key.service),
                "there is no container. Start it with `minato up`",
            )
        })?;

        if container.state.as_deref() != Some("running") {
            // Wanting to exec into a container is at its most likely just
            // after one fell over, and "start it with `minato up`" describes
            // the wrong problem.
            let detail = match crash_code(container.status.as_deref()) {
                Some(code) => format!(
                    "the container exited with code {code}. `minato logs {}` says why",
                    key.service
                ),
                None => "the container is not running. Start it with `minato up`".to_string(),
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

        Ok(containers
            .iter()
            .filter_map(Self::summary_to_status)
            .collect())
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

    fn minato_labels() -> HashMap<String, String> {
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
    fn reconstructs_state_from_labels() {
        // A daemon restart has nothing but this to recover its state
        // from.
        let status = DockerRuntime::summary_to_status(&summary_with(minato_labels(), "running"))
            .expect("recovers when the Minato labels are all there");

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
    fn ignores_containers_without_minato_labels() {
        let mut foreign = HashMap::new();
        foreign.insert("com.example.app".to_string(), "other".to_string());

        assert!(
            DockerRuntime::summary_to_status(&summary_with(foreign, "running")).is_none(),
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
            let status =
                DockerRuntime::summary_to_status(&summary_with(minato_labels(), docker_state))
                    .expect("recovers");
            assert_eq!(status.state, expected, "docker state = {docker_state}");
        }
    }

    /// A summary that also carries the `Status` line `docker ps` shows.
    fn exited_with(status: &str) -> ContainerSummary {
        ContainerSummary {
            status: Some(status.into()),
            ..summary_with(minato_labels(), "exited")
        }
    }

    #[test]
    fn a_container_that_fell_over_is_failed_not_stopped() {
        // The reason SKILL.md promises. Without it a start-up script that
        // died looks exactly like a service nobody started.
        let status = DockerRuntime::summary_to_status(&exited_with("Exited (127) 2 seconds ago"))
            .expect("recovers");

        let ServiceState::Failed { reason } = &status.state else {
            panic!("expected a failure, got {:?}", status.state);
        };
        assert!(reason.contains("127"), "name the exit code: {reason}");
        assert!(
            reason.contains("minato logs web"),
            "say where to look: {reason}"
        );
    }

    #[test]
    fn a_dead_container_is_failed_too() {
        let status = DockerRuntime::summary_to_status(&summary_with(minato_labels(), "dead"))
            .expect("recovers");

        let ServiceState::Failed { reason } = &status.state else {
            panic!("expected a failure, got {:?}", status.state);
        };
        assert!(reason.contains("minato logs web"), "{reason}");
    }

    #[test]
    fn anything_but_a_crash_stays_stopped() {
        // `docker stop` sends SIGTERM, and a shell exits 143 for it, so
        // every `minato down` would otherwise end in red. An unreadable or
        // absent status line must not be guessed at either.
        for line in [
            "Exited (0) 1 second ago",
            "Exited (143) 1 second ago",
            "Exited (137) 1 second ago",
            "Exited (oops) 1 second ago",
        ] {
            let status = DockerRuntime::summary_to_status(&exited_with(line)).expect("recovers");
            assert_eq!(status.state, ServiceState::Stopped, "{line}");
        }

        let no_status =
            DockerRuntime::summary_to_status(&summary_with(minato_labels(), "exited")).expect("ok");
        assert_eq!(no_status.state, ServiceState::Stopped, "no status line");
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
        let mut shared = minato_labels();
        shared.insert(labels::SCOPE.to_string(), "project".to_string());
        shared.insert(labels::WORKSPACE.to_string(), "_shared".to_string());

        let status =
            DockerRuntime::summary_to_status(&summary_with(shared, "running")).expect("recovers");

        assert_eq!(status.scope, ServiceScope::Project);
        assert!(status.key.workspace.is_shared());
    }

    #[test]
    fn endpoint_is_absent_when_port_not_published() {
        let mut no_port = minato_labels();
        no_port.remove(labels::PORT);

        let mut summary = summary_with(no_port, "running");
        summary.ports = None;

        let status = DockerRuntime::summary_to_status(&summary).expect("recovers");
        assert_eq!(status.port, None);
        assert_eq!(status.endpoint, None);
    }
}
