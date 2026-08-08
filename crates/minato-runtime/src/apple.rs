//! The Apple Container backend, via the `container` CLI.
//!
//! Three structural differences from Docker, each absorbed here.
//!
//! 1. **Every container gets its own IP** (`192.168.64.x`). No port
//!    forwarding is needed, so nothing is published and `endpoint` is the
//!    container's own address. Port collisions become impossible as a
//!    side effect.
//! 2. **No filtering by label.** The CLI has no filters, so the whole
//!    listing comes back and is narrowed down here.
//! 3. **Networks need macOS 26 or later.** Where they are unavailable,
//!    everything shares the default network.
//!
//! On top of that there are no network aliases, so a service name resolves
//! to nothing. `{container name}.test` does resolve, so it is injected as
//! `MINATO_HOST_<SERVICE>`.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};

use async_trait::async_trait;
use futures::stream::BoxStream;
use minato_api::OutputStream;
use minato_core::{ServiceScope, ServiceState};
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::error::{Result, RuntimeError};
use crate::event::EventSink;
use crate::health::{DEFAULT_READINESS_TIMEOUT, await_service};
use crate::runtime::{ExecOutcome, LogLine, LogOptions, Runtime, RuntimeInfo, labels, names};
use crate::spec::{
    RunningService, ServiceKey, ServiceSpec, ServiceStatus, SourceMount, VolumeMount, WorkspaceKey,
    WorkspaceSpec,
};

const RUNTIME_ID: &str = "apple";

/// The CLI to invoke.
const PROGRAM: &str = "container";

/// The DNS suffix Apple Container gives each container.
const DNS_SUFFIX: &str = "test";

pub struct AppleContainerRuntime {
    program: String,
    /// Where the storage behind a named volume actually lives.
    ///
    /// Apple Container has no notion of a named volume, so a host
    /// directory is bind-mounted to get the same persistence.
    volume_root: PathBuf,
    /// Whether custom networks work. 0 = unknown, 1 = yes, 2 = no.
    network_support: AtomicU8,
}

impl Default for AppleContainerRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl AppleContainerRuntime {
    pub fn new() -> Self {
        let volume_root = minato_core::Paths::resolve()
            .map(|paths| paths.root().join("volumes"))
            .unwrap_or_else(|_| PathBuf::from("/tmp/minato-volumes"));

        Self::with_settings(PROGRAM.to_string(), volume_root)
    }

    pub fn with_settings(program: String, volume_root: PathBuf) -> Self {
        Self {
            program,
            volume_root,
            network_support: AtomicU8::new(0),
        }
    }

    /// Runs the CLI and returns its stdout.
    async fn run(&self, args: &[&str]) -> Result<String> {
        let output = Command::new(&self.program)
            .args(args)
            .output()
            .await
            .map_err(|err| {
                if err.kind() == std::io::ErrorKind::NotFound {
                    RuntimeError::Unavailable {
                        runtime: RUNTIME_ID.to_string(),
                        message: format!(
                            "no `{}` command found. Install Apple Container",
                            self.program
                        ),
                    }
                } else {
                    RuntimeError::Unavailable {
                        runtime: RUNTIME_ID.to_string(),
                        message: err.to_string(),
                    }
                }
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(RuntimeError::Failed {
                operation: format!("{} {}", self.program, args.join(" ")),
                message: if stderr.is_empty() {
                    format!("exit code {}", output.status.code().unwrap_or(-1))
                } else {
                    stderr
                },
            });
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Only whether it worked. For cases where failure means "this
    /// feature is not available".
    async fn succeeds(&self, args: &[&str]) -> bool {
        Command::new(&self.program)
            .args(args)
            .output()
            .await
            .map(|out| out.status.success())
            .unwrap_or(false)
    }

    /// Every container Minato manages.
    ///
    /// The CLI cannot filter by label, so the whole listing comes back and
    /// is narrowed down here.
    async fn managed_containers(&self) -> Result<Vec<AppleContainerRecord>> {
        let json = self.run(&["ls", "--all", "--format", "json"]).await?;
        let records = parse_container_list(&json)?;

        Ok(records
            .into_iter()
            .filter(|record| {
                record
                    .configuration
                    .labels
                    .get(labels::MANAGED)
                    .map(String::as_str)
                    == Some(labels::MANAGED_VALUE)
            })
            .collect())
    }

    async fn find_container(&self, key: &ServiceKey) -> Result<Option<AppleContainerRecord>> {
        let name = names::container(key);
        Ok(self
            .managed_containers()
            .await?
            .into_iter()
            .find(|record| record.configuration.id == name))
    }

    /// Checks once whether custom networks work, and remembers.
    async fn supports_networks(&self) -> bool {
        match self.network_support.load(Ordering::Relaxed) {
            1 => return true,
            2 => return false,
            _ => {}
        }

        let supported = self
            .succeeds(&["network", "list", "--format", "json"])
            .await;
        self.network_support
            .store(if supported { 1 } else { 2 }, Ordering::Relaxed);
        supported
    }

    async fn ensure_network(
        &self,
        key: &WorkspaceKey,
        events: &EventSink,
    ) -> Result<Option<String>> {
        if !self.supports_networks().await {
            events.debug(
                "custom networks cannot be created here, so the default \
                 network is used (Apple Container needs macOS 26 or later \
                 for networks)",
            );
            return Ok(None);
        }

        let name = names::network(key);
        let existing = self.run(&["network", "list", "--format", "json"]).await?;

        if parse_network_names(&existing)?.iter().any(|n| n == &name) {
            return Ok(Some(name));
        }

        // A race that comes back as "already exists" counts as success.
        match self.run(&["network", "create", &name]).await {
            Ok(_) => Ok(Some(name)),
            Err(err) if err.to_string().contains("exists") => Ok(Some(name)),
            Err(err) => Err(err),
        }
    }

    /// Creates the host directory backing a named volume.
    fn ensure_volume_dir(&self, project: &str, name: &str) -> Result<PathBuf> {
        let path = self.volume_root.join(project).join(name);
        std::fs::create_dir_all(&path).map_err(|err| {
            RuntimeError::failed(format!("creating volume storage at {}", path.display()), err)
        })?;
        Ok(path)
    }

    /// The environment for this service, with its peers' hostnames added.
    fn env_with_peers(&self, spec: &ServiceSpec) -> BTreeMap<String, String> {
        let mut env = spec.env.clone();

        for peer in &spec.peers {
            let peer_key = spec.attached_to.service(peer);
            let hostname = format!("{}.{DNS_SUFFIX}", names::container(&peer_key));
            let var = format!("MINATO_HOST_{}", peer.to_uppercase().replace('-', "_"));

            // Never overwrite what the user set explicitly.
            env.entry(var).or_insert(hostname);
        }

        env
    }

    /// Builds the arguments for `container create`.
    fn create_args(&self, spec: &ServiceSpec, network: Option<&str>) -> Result<Vec<String>> {
        let name = names::container(&spec.key);
        let mut args: Vec<String> = vec!["create".into(), "--name".into(), name];

        args.push("--workdir".into());
        args.push(spec.workdir.clone());

        for (key, value) in self.env_with_peers(spec) {
            args.push("--env".into());
            args.push(format!("{key}={value}"));
        }

        for (key, value) in container_labels(spec) {
            args.push("--label".into());
            args.push(format!("{key}={value}"));
        }

        if let Some(network) = network {
            args.push("--network".into());
            args.push(network.to_string());
        }

        if let Some(SourceMount { host, target }) = &spec.source_mount {
            args.push("--volume".into());
            args.push(format!("{}:{}", host.display(), target));
        }

        for volume in &spec.volumes {
            let (source, target, read_only) = match volume {
                VolumeMount::Named {
                    name,
                    target,
                    read_only,
                } => {
                    let path = self.ensure_volume_dir(&spec.key.workspace.project, name)?;
                    (path, target.clone(), *read_only)
                }
                VolumeMount::Bind {
                    source,
                    target,
                    read_only,
                } => (source.clone(), target.clone(), *read_only),
            };

            args.push("--volume".into());
            args.push(if read_only {
                format!("{}:{}:ro", source.display(), target)
            } else {
                format!("{}:{}", source.display(), target)
            });
        }

        args.push(spec.image.clone());

        if let Some(command) = &spec.command {
            args.extend(command.iter().cloned());
        }

        Ok(args)
    }
}

/// The labels put on a container. The same keys as the Docker backend.
fn container_labels(spec: &ServiceSpec) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    map.insert(
        labels::MANAGED.to_string(),
        labels::MANAGED_VALUE.to_string(),
    );
    map.insert(
        labels::PROJECT.to_string(),
        spec.key.workspace.project.clone(),
    );
    map.insert(
        labels::WORKSPACE.to_string(),
        spec.key.workspace.workspace.clone(),
    );
    map.insert(labels::SERVICE.to_string(), spec.key.service.clone());
    map.insert(
        labels::SCOPE.to_string(),
        match spec.scope {
            ServiceScope::Workspace => "workspace".to_string(),
            ServiceScope::Project => "project".to_string(),
        },
    );
    if let Some(port) = spec.port {
        map.insert(labels::PORT.to_string(), port.to_string());
    }
    map
}

#[async_trait]
impl Runtime for AppleContainerRuntime {
    fn id(&self) -> &'static str {
        RUNTIME_ID
    }

    async fn probe(&self) -> Result<RuntimeInfo> {
        let version = self.run(&["--version"]).await?;

        // With the service down, everything after this fails.
        if !self.succeeds(&["system", "status"]).await {
            return Err(RuntimeError::Unavailable {
                runtime: RUNTIME_ID.to_string(),
                message: "the Apple Container service is not running".to_string(),
            });
        }

        Ok(RuntimeInfo {
            id: RUNTIME_ID.to_string(),
            version: parse_version(&version),
            supports_custom_networks: self.supports_networks().await,
        })
    }

    async fn prepare(&self, spec: &WorkspaceSpec, events: &EventSink) -> Result<()> {
        events.step_started("network", "preparing the network");
        self.ensure_network(&spec.key, events).await?;
        if spec
            .services
            .iter()
            .any(|s| s.scope == ServiceScope::Project)
        {
            self.ensure_network(&WorkspaceKey::shared(&spec.key.project), events)
                .await?;
        }
        events.step_done("network", "preparing the network");

        for service in &spec.services {
            let label = format!("pulling image {}", service.image);
            events.step_started("pull", &label);

            match self.run(&["image", "pull", &service.image]).await {
                Ok(_) => events.step_done("pull", &label),
                Err(err) => {
                    events.step_failed("pull", &label, err.to_string());
                    return Err(RuntimeError::ImageUnavailable {
                        image: service.image.clone(),
                        message: err.to_string(),
                    });
                }
            }
        }

        Ok(())
    }

    async fn start(&self, spec: &ServiceSpec, events: &EventSink) -> Result<RunningService> {
        let name = names::container(&spec.key);

        if let Some(existing) = self.find_container(&spec.key).await? {
            if existing.is_running() {
                events.step_skipped(
                    "start",
                    format!("starting {}", spec.name()),
                    "already running",
                );
                events.service_state(spec.name(), ServiceState::Ready);

                return Ok(RunningService {
                    key: spec.key.clone(),
                    container_id: existing.configuration.id.clone(),
                    endpoint: existing.endpoint(spec.port),
                });
            }

            // A stopped container may be carrying a stale configuration,
            // so recreate it.
            self.run(&["delete", "--force", &name]).await?;
        }

        events.step_started("start", format!("starting {}", spec.name()));
        events.service_state(spec.name(), ServiceState::Starting);

        let network = self.ensure_network(&spec.attached_to, events).await?;
        let args = self.create_args(spec, network.as_deref())?;
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();

        if let Err(err) = self.run(&arg_refs).await {
            events.step_failed("start", format!("starting {}", spec.name()), err.to_string());
            return Err(err);
        }

        if let Err(err) = self.run(&["start", &name]).await {
            events.step_failed("start", format!("starting {}", spec.name()), err.to_string());
            return Err(err);
        }

        // The IP is only assigned once the container is up, so read it
        // after starting.
        let record = self.find_container(&spec.key).await?;
        let endpoint = record.as_ref().and_then(|r| r.endpoint(spec.port));

        if spec.port.is_some() && endpoint.is_none() {
            events.warn(format!(
                "cannot read {}'s IP address yet; waiting for the network \
                 to be assigned",
                spec.name()
            ));
        }

        events.step_done("start", format!("starting {}", spec.name()));

        // For the same reason as the Docker backend: confirm it answers
        // before declaring it ready.
        await_service(
            spec.name(),
            endpoint,
            spec.health.as_ref(),
            DEFAULT_READINESS_TIMEOUT,
            events,
        )
        .await;

        events.service_state(spec.name(), ServiceState::Ready);

        Ok(RunningService {
            key: spec.key.clone(),
            container_id: name,
            endpoint,
        })
    }

    async fn stop(&self, key: &ServiceKey, events: &EventSink) -> Result<()> {
        let Some(record) = self.find_container(key).await? else {
            events.step_skipped(
                "stop",
                format!("stopping {}", key.service),
                "not running",
            );
            return Ok(());
        };

        if !record.is_running() {
            events.step_skipped(
                "stop",
                format!("stopping {}", key.service),
                "not running",
            );
            return Ok(());
        }

        events.step_started("stop", format!("stopping {}", key.service));
        self.run(&["stop", &names::container(key)]).await?;
        events.step_done("stop", format!("stopping {}", key.service));
        events.service_state(&key.service, ServiceState::Stopped);
        Ok(())
    }

    async fn remove(&self, key: &ServiceKey, events: &EventSink) -> Result<()> {
        if self.find_container(key).await?.is_none() {
            return Ok(());
        }

        events.step_started("remove", format!("removing {}", key.service));
        self.run(&["delete", "--force", &names::container(key)])
            .await?;
        events.step_done("remove", format!("removing {}", key.service));
        Ok(())
    }

    async fn destroy_workspace(&self, key: &WorkspaceKey, events: &EventSink) -> Result<()> {
        let containers = self.managed_containers().await?;
        let targets: Vec<String> = containers
            .iter()
            .filter(|record| {
                record.label(labels::PROJECT) == Some(key.project.as_str())
                    && record.label(labels::WORKSPACE) == Some(key.workspace.as_str())
            })
            .map(|record| record.configuration.id.clone())
            .collect();

        events.step_started("destroy", "removing containers");
        for name in targets {
            self.run(&["delete", "--force", &name]).await?;
        }
        events.step_done("destroy", "removing containers");

        if self.supports_networks().await {
            let network = names::network(key);
            if !self.succeeds(&["network", "delete", &network]).await {
                events.debug(format!("network {network} was not removed"));
            }
        }

        Ok(())
    }

    async fn inspect(&self, key: &ServiceKey) -> Result<ServiceStatus> {
        match self.find_container(key).await? {
            Some(record) => Ok(record.to_status()),
            None => Ok(ServiceStatus::stopped(key.clone(), ServiceScope::Workspace)),
        }
    }

    async fn logs(
        &self,
        key: &ServiceKey,
        options: LogOptions,
    ) -> Result<BoxStream<'static, LogLine>> {
        let name = names::container(key);
        let mut args: Vec<String> = vec!["logs".into()];

        if options.follow {
            args.push("--follow".into());
        }
        if let Some(tail) = options.tail {
            args.push("-n".into());
            args.push(tail.to_string());
        }
        args.push(name);

        // It is a CLI, so read the child's stdout line by line.
        let mut child = Command::new(&self.program)
            .args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|err| RuntimeError::failed(format!("reading logs for {}", key.service), err))?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();

        if let Some(stdout) = stdout {
            let sender = sender.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if sender
                        .send(LogLine {
                            stream: OutputStream::Stdout,
                            line,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }

        if let Some(stderr) = stderr {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if sender
                        .send(LogLine {
                            stream: OutputStream::Stderr,
                            line,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }

        // Reap the child once the reader stops. Left alone, a `follow`
        // would keep its process around forever.
        tokio::spawn(async move {
            let _ = child.wait().await;
        });

        Ok(Box::pin(
            tokio_stream::wrappers::UnboundedReceiverStream::new(receiver),
        ))
    }

    async fn exec(
        &self,
        key: &ServiceKey,
        command: &[String],
        events: &EventSink,
    ) -> Result<ExecOutcome> {
        let Some(record) = self.find_container(key).await? else {
            return Err(RuntimeError::failed(
                format!("running a command in {}", key.service),
                "there is no container. Start it with `minato up`",
            ));
        };

        if !record.is_running() {
            return Err(RuntimeError::failed(
                format!("running a command in {}", key.service),
                "the container is not running. Start it with `minato up`",
            ));
        }

        let name = names::container(key);
        let mut args: Vec<String> = vec!["exec".into(), name];
        args.extend(command.iter().cloned());

        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();

        let output = Command::new(&self.program)
            .args(&arg_refs)
            .output()
            .await
            .map_err(|err| {
                RuntimeError::failed(format!("running a command in {}", key.service), err)
            })?;

        for line in String::from_utf8_lossy(&output.stdout).lines() {
            events.output(Some(key.service.clone()), OutputStream::Stdout, line);
        }
        for line in String::from_utf8_lossy(&output.stderr).lines() {
            events.output(Some(key.service.clone()), OutputStream::Stderr, line);
        }

        Ok(ExecOutcome {
            exit_code: output.status.code().unwrap_or(-1),
        })
    }

    async fn list_project(&self, project: &str) -> Result<Vec<ServiceStatus>> {
        Ok(self
            .managed_containers()
            .await?
            .into_iter()
            .filter(|record| record.label(labels::PROJECT) == Some(project))
            .map(|record| record.to_status())
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Parsing the CLI output
//
// `container` returns JSON shaped like this (from docs/how-to.md):
//
// [{ "status": "running",
//    "networks": [{"address": "192.168.64.3/24", "gateway": "192.168.64.1",
//                  "hostname": "my-web-server.test.", "network": "default"}],
//    "configuration": {"id": "my-web-server", "labels": {...}, ...} }]
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct AppleContainerRecord {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub networks: Vec<AppleNetworkAttachment>,
    pub configuration: AppleConfiguration,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppleNetworkAttachment {
    /// Comes back with the CIDR attached (`192.168.64.3/24`).
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub network: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppleConfiguration {
    /// The container name, as passed to `--name`.
    pub id: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub image: Option<AppleImage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppleImage {
    #[serde(default)]
    pub reference: Option<String>,
}

impl AppleContainerRecord {
    pub fn is_running(&self) -> bool {
        self.status.as_deref() == Some("running")
    }

    pub fn label(&self, key: &str) -> Option<&str> {
        self.configuration.labels.get(key).map(String::as_str)
    }

    /// The IPv4 address assigned on the first network.
    pub fn ip(&self) -> Option<Ipv4Addr> {
        self.networks
            .iter()
            .find_map(|net| net.address.as_deref())
            .and_then(parse_cidr_address)
    }

    /// Where the proxy forwards to: the container's own address, not a
    /// forwarded port.
    pub fn endpoint(&self, port: Option<u16>) -> Option<SocketAddr> {
        let port = port.or_else(|| {
            self.label(labels::PORT)
                .and_then(|value| value.parse::<u16>().ok())
        })?;

        // A stopped container has no IP.
        if !self.is_running() {
            return None;
        }

        self.ip().map(|ip| SocketAddr::new(IpAddr::V4(ip), port))
    }

    pub fn state(&self) -> ServiceState {
        match self.status.as_deref() {
            Some("running") => ServiceState::Ready,
            Some("stopping") | Some("starting") => ServiceState::Starting,
            Some("stopped") | Some("exited") => ServiceState::Stopped,
            Some(_) => ServiceState::Unknown,
            None => ServiceState::Unknown,
        }
    }

    pub fn to_status(&self) -> ServiceStatus {
        let project = self.label(labels::PROJECT).unwrap_or_default().to_string();
        let workspace = self
            .label(labels::WORKSPACE)
            .unwrap_or_default()
            .to_string();
        let service = self.label(labels::SERVICE).unwrap_or_default().to_string();

        let scope = match self.label(labels::SCOPE) {
            Some("project") => ServiceScope::Project,
            _ => ServiceScope::Workspace,
        };

        let port = self
            .label(labels::PORT)
            .and_then(|value| value.parse::<u16>().ok());

        ServiceStatus {
            key: WorkspaceKey::new(project, workspace).service(service),
            state: self.state(),
            container_id: Some(self.configuration.id.clone()),
            image: self
                .configuration
                .image
                .as_ref()
                .and_then(|img| img.reference.clone()),
            endpoint: self.endpoint(port),
            port,
            scope,
        }
    }
}

fn parse_container_list(json: &str) -> Result<Vec<AppleContainerRecord>> {
    if json.trim().is_empty() {
        return Ok(Vec::new());
    }

    serde_json::from_str(json).map_err(|err| RuntimeError::Failed {
        operation: "parsing the output of container ls".to_string(),
        message: format!("{err}\noutput: {json}"),
    })
}

fn parse_network_names(json: &str) -> Result<Vec<String>> {
    if json.trim().is_empty() {
        return Ok(Vec::new());
    }

    #[derive(Deserialize)]
    struct NetworkRecord {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        name: Option<String>,
    }

    let records: Vec<NetworkRecord> =
        serde_json::from_str(json).map_err(|err| RuntimeError::Failed {
            operation: "parsing the output of container network list".to_string(),
            message: format!("{err}\noutput: {json}"),
        })?;

    Ok(records
        .into_iter()
        .filter_map(|record| record.name.or(record.id))
        .collect())
}

/// Takes just the IP out of `192.168.64.3/24`.
fn parse_cidr_address(address: &str) -> Option<Ipv4Addr> {
    address
        .split('/')
        .next()
        .and_then(|ip| ip.parse::<Ipv4Addr>().ok())
}

/// Picks the version out of `container CLI version 0.5.0 (build ...)`.
fn parse_version(output: &str) -> String {
    output
        .split_whitespace()
        .find(|token| token.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .unwrap_or("unknown")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Modelled on the real output in docs/how-to.md.
    const SAMPLE: &str = r#"[
      {
        "status": "running",
        "networks": [
          {
            "address": "192.168.64.3/24",
            "gateway": "192.168.64.1",
            "hostname": "minato-myapp-feat-1-web.test.",
            "network": "default"
          }
        ],
        "configuration": {
          "id": "minato-myapp-feat-1-web",
          "hostname": "minato-myapp-feat-1-web",
          "mounts": [],
          "labels": {
            "dev.minato.managed": "1",
            "dev.minato.project": "myapp",
            "dev.minato.workspace": "feat-1",
            "dev.minato.service": "web",
            "dev.minato.scope": "workspace",
            "dev.minato.port": "3000"
          },
          "image": { "reference": "node:22" },
          "resources": { "cpus": 4, "memoryInBytes": 1073741824 }
        }
      }
    ]"#;

    #[test]
    fn parses_real_cli_output() {
        let records = parse_container_list(SAMPLE).expect("parses");
        assert_eq!(records.len(), 1);

        let record = &records[0];
        assert!(record.is_running());
        assert_eq!(record.configuration.id, "minato-myapp-feat-1-web");
        assert_eq!(record.label(labels::SERVICE), Some("web"));
        assert_eq!(record.ip(), Some(Ipv4Addr::new(192, 168, 64, 3)));
    }

    #[test]
    fn endpoint_points_at_the_container_not_localhost() {
        // Unlike Docker, the connection goes straight to the container
        // with no forwarded port in between. Absorbing that difference is
        // what RunningService::endpoint is for.
        let record = &parse_container_list(SAMPLE).expect("parses")[0];

        assert_eq!(
            record.endpoint(Some(3000)),
            Some(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(192, 168, 64, 3)),
                3000
            ))
        );
    }

    #[test]
    fn endpoint_falls_back_to_port_label() {
        let record = &parse_container_list(SAMPLE).expect("parses")[0];
        // The port can be recovered from the label even with no spec
        // to hand.
        assert_eq!(
            record.endpoint(None).map(|addr| addr.port()),
            Some(3000),
            "the port must survive a daemon restart"
        );
    }

    #[test]
    fn stopped_container_has_no_endpoint() {
        let json = SAMPLE.replace(r#""status": "running""#, r#""status": "stopped""#);
        let record = &parse_container_list(&json).expect("parses")[0];

        assert_eq!(record.state(), ServiceState::Stopped);
        assert_eq!(record.endpoint(Some(3000)), None, "a stopped container has no usable IP");
    }

    #[test]
    fn converts_to_service_status() {
        let record = &parse_container_list(SAMPLE).expect("parses")[0];
        let status = record.to_status();

        assert_eq!(status.key.workspace.project, "myapp");
        assert_eq!(status.key.workspace.workspace, "feat-1");
        assert_eq!(status.key.service, "web");
        assert_eq!(status.state, ServiceState::Ready);
        assert_eq!(status.port, Some(3000));
        assert_eq!(status.image.as_deref(), Some("node:22"));
        assert_eq!(status.scope, ServiceScope::Workspace);
    }

    #[test]
    fn tolerates_missing_optional_fields() {
        // Keeps working across CLI output changes, as long as the
        // required fields are there.
        let json = r#"[{"configuration": {"id": "x"}}]"#;
        let records = parse_container_list(json).expect("parses");

        assert_eq!(records.len(), 1);
        assert!(!records[0].is_running());
        assert_eq!(records[0].ip(), None);
        assert_eq!(records[0].state(), ServiceState::Unknown);
    }

    #[test]
    fn parses_empty_list() {
        assert!(parse_container_list("[]").expect("parses").is_empty());
        assert!(parse_container_list("").expect("empty is fine").is_empty());
    }

    #[test]
    fn reports_unparseable_output_with_content() {
        let err = parse_container_list("not json").unwrap_err();
        assert!(
            err.to_string().contains("not json"),
            "the output has to be there to debug with: {err}"
        );
    }

    #[test]
    fn strips_cidr_suffix() {
        assert_eq!(
            parse_cidr_address("192.168.64.3/24"),
            Some(Ipv4Addr::new(192, 168, 64, 3))
        );
        assert_eq!(
            parse_cidr_address("10.0.0.1"),
            Some(Ipv4Addr::new(10, 0, 0, 1))
        );
        assert_eq!(parse_cidr_address("garbage"), None);
    }

    #[test]
    fn extracts_version_number() {
        assert_eq!(
            parse_version("container CLI version 0.5.0 (build 1)"),
            "0.5.0"
        );
        assert_eq!(parse_version("0.1.0"), "0.1.0");
        assert_eq!(parse_version("no digits here"), "unknown");
    }

    #[test]
    fn parses_network_list() {
        let json = r#"[{"name": "minato-myapp-feat-1"}, {"id": "default"}]"#;
        let names = parse_network_names(json).expect("parses");
        assert_eq!(names, vec!["minato-myapp-feat-1", "default"]);
    }

    fn spec_with_peers(peers: Vec<String>) -> ServiceSpec {
        ServiceSpec {
            key: WorkspaceKey::new("myapp", "feat-1").service("api"),
            attached_to: WorkspaceKey::new("myapp", "feat-1"),
            image: "node:22".into(),
            command: None,
            workdir: "/workspace".into(),
            env: BTreeMap::new(),
            port: Some(8080),
            health: None,
            scope: ServiceScope::Workspace,
            volumes: vec![],
            source_mount: None,
            peers,
        }
    }

    #[test]
    fn injects_peer_hostnames() {
        // No aliases, so the peer's container name is passed in the
        // environment.
        let runtime = AppleContainerRuntime::with_settings(
            PROGRAM.into(),
            PathBuf::from("/tmp/minato-test-volumes"),
        );
        let env = runtime.env_with_peers(&spec_with_peers(vec!["db".into(), "cache-store".into()]));

        assert_eq!(
            env.get("MINATO_HOST_DB").map(String::as_str),
            Some("minato-myapp-feat-1-db.test")
        );
        assert_eq!(
            env.get("MINATO_HOST_CACHE_STORE").map(String::as_str),
            Some("minato-myapp-feat-1-cache-store.test"),
            "a hyphen is not valid in an environment variable name"
        );
    }

    #[test]
    fn does_not_override_user_supplied_env() {
        let runtime = AppleContainerRuntime::with_settings(
            PROGRAM.into(),
            PathBuf::from("/tmp/minato-test-volumes"),
        );

        let mut spec = spec_with_peers(vec!["db".into()]);
        spec.env
            .insert("MINATO_HOST_DB".into(), "custom-host".into());

        let env = runtime.env_with_peers(&spec);
        assert_eq!(
            env.get("MINATO_HOST_DB").map(String::as_str),
            Some("custom-host")
        );
    }

    #[test]
    fn builds_create_args_without_publishing_ports() {
        let runtime = AppleContainerRuntime::with_settings(
            PROGRAM.into(),
            PathBuf::from("/tmp/minato-test-volumes"),
        );

        let mut spec = spec_with_peers(vec![]);
        spec.command = Some(vec!["pnpm".into(), "dev".into()]);
        spec.source_mount = Some(SourceMount {
            host: PathBuf::from("/repo/wt/feat-1"),
            target: "/workspace".into(),
        });

        let args = runtime
            .create_args(&spec, Some("minato-myapp-feat-1"))
            .expect("builds");

        assert_eq!(args[0], "create");
        assert!(
            !args.iter().any(|a| a == "--publish" || a == "-p"),
            "every container has its own IP, so nothing needs publishing: {args:?}"
        );

        // The command has to come after the image.
        let image_index = args
            .iter()
            .position(|a| a == "node:22")
            .expect("has the image");
        let cmd_index = args
            .iter()
            .position(|a| a == "pnpm")
            .expect("has the command");
        assert!(image_index < cmd_index, "image then command: {args:?}");

        assert!(
            args.windows(2)
                .any(|w| w[0] == "--volume" && w[1] == "/repo/wt/feat-1:/workspace"),
            "the worktree is mounted: {args:?}"
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--label" && w[1] == "dev.minato.service=api"),
            "the labels are there: {args:?}"
        );
    }

    #[test]
    fn maps_named_volumes_to_host_directories() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime =
            AppleContainerRuntime::with_settings(PROGRAM.into(), dir.path().to_path_buf());

        let mut spec = spec_with_peers(vec![]);
        spec.volumes = vec![VolumeMount::Named {
            name: "pgdata".into(),
            target: "/var/lib/postgresql/data".into(),
            read_only: false,
        }];

        let args = runtime.create_args(&spec, None).expect("builds");
        let expected = dir.path().join("myapp").join("pgdata");

        assert!(
            args.windows(2).any(|w| {
                w[0] == "--volume"
                    && w[1] == format!("{}:/var/lib/postgresql/data", expected.display())
            }),
            "a named volume maps to a host directory: {args:?}"
        );
        assert!(expected.is_dir(), "the directory behind it is created");
    }
}
