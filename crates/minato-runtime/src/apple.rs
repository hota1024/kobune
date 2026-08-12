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
//! On top of that **there is no container-to-container name resolution at
//! all** — no aliases, and no DNS either. A container's nameserver is its
//! network gateway, which answers NXDOMAIN for every container name. So a
//! peer's IP address is injected as `MINATO_HOST_<SERVICE>` instead.
//!
//! That last one is why this backend leaves
//! [`Runtime::starts_concurrently`] alone: an address can only be read off
//! a peer that is already running, so services started side by side would
//! each be handed nothing for the other. Docker has the DNS that makes the
//! question moot; here the sequence *is* the mechanism.

use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use minato_api::OutputStream;
use minato_core::{ServiceScope, ServiceState};
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::error::{Result, RuntimeError};
use crate::event::EventSink;
use crate::health::{DEFAULT_READINESS_TIMEOUT, await_service};
use crate::runtime::{
    Attachment, ExecOptions, ExecOutcome, LogLine, LogOptions, Runtime, RuntimeInfo, Sizing,
    Throwaway, labels, names,
};
use crate::spec::{
    BuildSpec, RunningService, ServiceKey, ServiceSpec, ServiceStatus, SourceMount, VolumeMount,
    WorkspaceKey, WorkspaceSpec,
};
use crate::terminal::Terminal;

const RUNTIME_ID: &str = "apple";

/// The CLI to invoke.
const PROGRAM: &str = minato_core::apple::PROGRAM;

/// Where the generated `/etc/hosts` files live, under the volume storage.
///
/// A leading `_` is not a valid label, so no project can take this name.
const HOSTS_DIR: &str = "_hosts";

/// How long to wait for a container started on a terminal to come up.
///
/// The attached start command never returns, so this is what stands in for
/// its exit code. Generous: the image is already there by this point, but
/// a cold `container` daemon can take a few seconds to answer.
const TERMINAL_START_TIMEOUT: Duration = Duration::from_secs(30);

/// How soon to ask whether it is up yet, and the longest that wait grows
/// to.
///
/// Each answer costs a `container ls` — a process, the whole listing, and
/// parsing it — so a fixed 100ms would spend hundreds of them on a start
/// that takes seconds. Doubling reaches a second after five tries and
/// leaves a fast start still noticing in a tenth of one.
const TERMINAL_START_POLL: Duration = Duration::from_millis(100);
const TERMINAL_START_POLL_MAX: Duration = Duration::from_secs(1);

pub struct AppleContainerRuntime {
    program: String,
    /// Where the storage behind a named volume actually lives.
    ///
    /// Apple Container has no notion of a named volume, so a host
    /// directory is bind-mounted to get the same persistence.
    volume_root: PathBuf,
    /// Whether custom networks work. 0 = unknown, 1 = yes, 2 = no.
    network_support: AtomicU8,
    /// The open terminal of every service started with `tty`.
    ///
    /// **Held from the moment the service starts.** There is no `attach`
    /// in the CLI, so the only way to reach a container's stdin is to be
    /// the one that started it (`terminal.rs`). A daemon that restarts
    /// therefore loses the terminals of services it did not start, and
    /// says so rather than pretending otherwise.
    terminals: Mutex<HashMap<ServiceKey, Terminal>>,
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
            terminals: Mutex::new(HashMap::new()),
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

    /// Lets go of a service's terminal. Its container has gone.
    fn forget_terminal(&self, key: &ServiceKey) {
        self.terminals.lock().expect("lock").remove(key);
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
        _key: &WorkspaceKey,
        events: &EventSink,
    ) -> Result<Option<String>> {
        // Everything shares the default network, even where macOS 26 would
        // let a per-workspace one be created.
        //
        // A container can only be on one network here — `--network` takes a
        // single value and there is no `network connect` — and containers
        // on different networks cannot reach each other. A shared service
        // (`scope = "project"`) attaches to whichever workspace started it
        // and is then unreachable from every other one, which was verified
        // and is exactly what `scope = "project"` exists to avoid.
        //
        // Per-workspace networks would buy isolation and nothing else:
        // there is no container DNS to scope, so names are not involved.
        // Correctness for shared services is worth more than isolation
        // between worktrees the same person owns. Revisit if Apple
        // Container gains multi-network attachment.
        if self.supports_networks().await {
            events.debug(
                "using the default network: Apple Container attaches a \
                 container to one network only, and a shared service on a \
                 per-workspace network is unreachable from the others",
            );
        }

        Ok(None)
    }

    /// Creates a per-workspace network.
    ///
    /// Unused while [`Self::ensure_network`] puts everything on the default
    /// network, and kept for when a container can hold more than one
    /// attachment.
    #[allow(dead_code, reason = "waiting on multi-network attachment")]
    async fn create_network(&self, key: &WorkspaceKey) -> Result<String> {
        let name = names::network(key);
        let existing = self.run(&["network", "list", "--format", "json"]).await?;

        if parse_network_names(&existing)?.iter().any(|n| n == &name) {
            return Ok(name);
        }

        // A race that comes back as "already exists" counts as success.
        match self.run(&["network", "create", &name]).await {
            Ok(_) => Ok(name),
            Err(err) if err.to_string().contains("exists") => Ok(name),
            Err(err) => Err(err),
        }
    }

    /// Builds the image unless that exact one is already here.
    ///
    /// The tag carries a fingerprint of the inputs, so an existing tag means
    /// an image built from exactly this Dockerfile and these args. Skipping
    /// matters most for scale-to-zero, where `prepare` sits in the path of
    /// the request that woke the service.
    ///
    /// Unlike Docker, the context is not packed and sent: `container build`
    /// takes a directory.
    async fn ensure_built(
        &self,
        build: &BuildSpec,
        rebuild: bool,
        events: &EventSink,
    ) -> Result<()> {
        let label = format!("building {}", build.tag);

        if !rebuild && self.image_exists(&build.tag).await {
            events.step_skipped("build", label, "already built");
            return Ok(());
        }

        events.step_started("build", &label);

        let context = build.context.to_string_lossy().to_string();
        let dockerfile = build.dockerfile.to_string_lossy().to_string();

        let mut args: Vec<String> = vec![
            "build".into(),
            "--tag".into(),
            build.tag.clone(),
            "--file".into(),
            dockerfile,
        ];

        for (key, value) in &build.args {
            args.push("--build-arg".into());
            args.push(format!("{key}={value}"));
        }

        args.push(context);

        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();

        match self.run(&arg_refs).await {
            Ok(_) => {
                events.step_done("build", &label);
                Ok(())
            }
            Err(err) => {
                events.step_failed("build", &label, err.to_string());
                Err(err)
            }
        }
    }

    /// Whether an image with this tag is already present.
    async fn image_exists(&self, tag: &str) -> bool {
        let Ok(json) = self.run(&["image", "list", "--format", "json"]).await else {
            return false;
        };

        parse_image_tags(&json)
            .unwrap_or_default()
            .iter()
            .any(|existing| existing == tag)
    }

    /// Creates the host directory backing a named volume.
    /// Apple Container has no named volumes, so they become directories.
    ///
    /// The layout mirrors the Docker naming so the two runtimes agree on
    /// what is shared with what: a workspace-scoped volume gets its own
    /// directory per worktree.
    fn ensure_volume_dir(
        &self,
        key: &WorkspaceKey,
        name: &str,
        scope: crate::spec::VolumeScope,
    ) -> Result<PathBuf> {
        // **One directory per volume, never one inside another.** Nesting
        // the worktree would put `cache@workspace` inside a project volume
        // that happened to be named after that worktree, so clearing the
        // one would take the other's storage with it.
        //
        // `.` separates, for the reason `names::volume` gives: it cannot
        // occur in a label, so `{worktree}.{name}` can never equal a bare
        // project volume's name. Project paths are untouched, so nothing
        // already stored moves.
        let path = match scope {
            crate::spec::VolumeScope::Project => self.volume_root.join(&key.project).join(name),
            crate::spec::VolumeScope::Workspace => self.volume_root.join(&key.project).join(
                format!("{}.{name}", names::sanitize_segment(&key.workspace)),
            ),
        };
        std::fs::create_dir_all(&path).map_err(|err| {
            RuntimeError::failed(
                format!("creating volume storage at {}", path.display()),
                err,
            )
        })?;
        Ok(path)
    }

    /// Removes the storage that belonged to this worktree and nothing else.
    ///
    /// **Workspace-scoped only**, for the reason the Docker backend gives:
    /// a project volume is shared and outlives any worktree, while this one
    /// is storage for a worktree being destroyed.
    ///
    /// There are no labels to filter on here, only directory names, and
    /// `{worktree}.` is the prefix `ensure_volume_dir` writes them under.
    fn remove_workspace_volumes(&self, key: &WorkspaceKey, events: &EventSink) {
        let project_dir = self.volume_root.join(&key.project);
        let prefix = format!("{}.", names::sanitize_segment(&key.workspace));

        let Ok(entries) = std::fs::read_dir(&project_dir) else {
            return;
        };

        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };

            if !name.starts_with(&prefix) {
                continue;
            }

            let path = entry.path();
            match std::fs::remove_dir_all(&path) {
                Ok(()) => events.debug(format!("removed volume storage {}", path.display())),
                Err(err) => {
                    events.debug(format!("{} was not removed: {err}", path.display()));
                }
            }
        }
    }

    /// Removes the generated `/etc/hosts` files of a workspace's services.
    ///
    /// They are named after the container, so the workspace's own prefix is
    /// what tells them from another worktree's. Nothing depends on this —
    /// each is rewritten on every start — but a destroyed worktree should
    /// not leave files behind.
    fn remove_workspace_hosts_files(&self, key: &WorkspaceKey, events: &EventSink) {
        let prefix = names::container(&key.service(""));

        let Ok(entries) = std::fs::read_dir(self.hosts_root()) else {
            return;
        };

        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };

            if !name.starts_with(&prefix) {
                continue;
            }

            let path = entry.path();
            if let Err(err) = std::fs::remove_file(&path) {
                events.debug(format!("{} was not removed: {err}", path.display()));
            }
        }
    }

    /// The environment for this service, with its peers' addresses added.
    ///
    /// **Addresses, not hostnames.** Apple Container 1.2.1 has no
    /// container-to-container name resolution: a container's nameserver is
    /// the network gateway, which answers NXDOMAIN for every container
    /// name, on the default network as much as a custom one. The `.test`
    /// domain M0 injected is a *host* resolver — it needs
    /// `sudo container system dns create` and, once created, lets the host
    /// reach containers, not containers reach each other. So a name was
    /// injected that nothing inside the container could ever resolve.
    ///
    /// A peer that is not running yet contributes nothing. Leaving the
    /// variable unset makes the app fail on a missing variable, which
    /// points at the ordering; a name that never resolves would send
    /// someone hunting for a DNS problem that does not exist. Declare
    /// `depends_on` and the peer will be up first.
    fn env_with_peers(
        &self,
        spec: &ServiceSpec,
        peer_addresses: &BTreeMap<String, Ipv4Addr>,
    ) -> BTreeMap<String, String> {
        let mut env = spec.env.clone();

        for peer in &spec.peers {
            let Some(address) = peer_addresses.get(peer) else {
                continue;
            };

            let var = format!("MINATO_HOST_{}", peer.to_uppercase().replace('-', "_"));

            // Never overwrite what the user set explicitly.
            env.entry(var).or_insert_with(|| address.to_string());
        }

        env
    }

    /// Where a container reaches the host, if the network can say.
    ///
    /// The gateway of the network everything is attached to. There is no
    /// `host.docker.internal` here and nothing forwards the host's
    /// loopback, so this is the only address a container can find the
    /// proxy at.
    async fn network_gateway(&self) -> Option<Ipv4Addr> {
        let listed = self.run(&minato_core::apple::LIST_ARGS).await.ok()?;

        minato_core::apple::parse_gateway(&listed, minato_core::apple::DEFAULT_NETWORK)
    }

    /// Writes the `/etc/hosts` this container gets, and says where it is.
    ///
    /// **Apple Container has no `--add-host`.** `container run --help` in
    /// 1.2.1 offers `--dns` and nothing else, and pointing the whole
    /// resolver at Minato's DNS would answer NXDOMAIN for every name
    /// outside `.localhost` — the container would lose the internet to
    /// gain an internal URL. Mounting the file Docker's flag writes for
    /// itself is the same thing without that cost.
    ///
    /// The file is written whole, since a mount replaces what the image
    /// had. `localhost` has to stay, and the container's own name is
    /// pointed at loopback the way Debian does it — the address it would
    /// otherwise carry is not known until the container has started, which
    /// is after this is needed.
    ///
    /// **One file per service, not per container.** A throwaway is named
    /// for the instant it was created, so keying on that would leave a
    /// file behind for every `minato exec` ever run, with nothing to sweep
    /// them: the workspace's own prefix does not match them. The contents
    /// are the same either way.
    ///
    /// It is moved into place rather than written in place, so a service
    /// container holding the file mounted cannot read a half-written one
    /// while an `exec` beside it rewrites the same path.
    ///
    /// `None` when there is nothing to map, which leaves the image's own
    /// file alone.
    fn write_hosts_file(&self, spec: &ServiceSpec, gateway: Ipv4Addr) -> Result<PathBuf> {
        let name = names::container(&spec.key);

        let dir = self.hosts_root();
        std::fs::create_dir_all(&dir)
            .map_err(|err| RuntimeError::failed(format!("creating {}", dir.display()), err))?;

        let mut contents = String::from(
            "# Written by Minato. The gateway is the host, where the proxy\n\
             # listens, so MINATO_URL_<SERVICE> reaches it from in here too.\n\
             127.0.0.1\tlocalhost\n\
             ::1\tlocalhost ip6-localhost ip6-loopback\n",
        );
        contents.push_str(&format!("127.0.1.1\t{name}\n"));

        for host in &spec.gateway_hosts {
            contents.push_str(&format!("{gateway}\t{host}\n"));
        }

        let path = dir.join(&name);
        let staged = dir.join(format!(".{name}.{}", std::process::id()));

        std::fs::write(&staged, contents)
            .map_err(|err| RuntimeError::failed(format!("writing {}", staged.display()), err))?;
        std::fs::rename(&staged, &path)
            .map_err(|err| RuntimeError::failed(format!("writing {}", path.display()), err))?;

        Ok(path)
    }

    /// Where the generated `/etc/hosts` files are kept.
    ///
    /// Beside the volume storage, under a name no project can take: a
    /// leading `_` is not a valid label, which is the same argument
    /// [`crate::spec::SHARED_WORKSPACE`] makes for itself.
    fn hosts_root(&self) -> PathBuf {
        self.volume_root.join(HOSTS_DIR)
    }

    /// The addresses of this service's peers that are already running.
    async fn peer_addresses(&self, spec: &ServiceSpec) -> Result<BTreeMap<String, Ipv4Addr>> {
        if spec.peers.is_empty() {
            return Ok(BTreeMap::new());
        }

        let records = self.managed_containers().await?;
        let mut addresses = BTreeMap::new();

        for peer in &spec.peers {
            // A shared service lives under the shared workspace, so match
            // on the labels rather than reconstructing the container name.
            let found = records.iter().find(|record| {
                record.label(labels::PROJECT) == Some(spec.key.workspace.project.as_str())
                    && record.label(labels::SERVICE) == Some(peer.as_str())
                    && record.is_running()
            });

            if let Some(address) = found.and_then(|record| record.ip()) {
                addresses.insert(peer.clone(), address);
            }
        }

        Ok(addresses)
    }

    /// Starts a service that asked for a terminal, and keeps the near end.
    ///
    /// `container start --attach --interactive` runs for as long as the
    /// container does, so unlike the plain `start` there is nothing to
    /// wait for. It is spawned instead, and the container itself is what
    /// says whether the start worked.
    async fn start_on_a_terminal(&self, spec: &ServiceSpec, name: &str) -> Result<()> {
        let mut command = tokio::process::Command::new(&self.program);
        command.args(["start", "--attach", "--interactive", name]);

        let terminal = Terminal::open(command).map_err(|err| {
            RuntimeError::failed(format!("opening a terminal for {}", spec.name()), err)
        })?;

        self.terminals
            .lock()
            .expect("lock")
            .insert(spec.key.clone(), terminal);

        let deadline = std::time::Instant::now() + TERMINAL_START_TIMEOUT;
        let mut wait = TERMINAL_START_POLL;

        loop {
            if self
                .find_container(&spec.key)
                .await?
                .is_some_and(|record| record.is_running())
            {
                return Ok(());
            }

            // The command has gone. Whatever it said went to the terminal
            // rather than to a pipe this could read, and the container is
            // not running, so all that is left is to say which it was.
            let gone = !self
                .terminals
                .lock()
                .expect("lock")
                .get(&spec.key)
                .is_some_and(Terminal::is_open);

            if gone || std::time::Instant::now() >= deadline {
                self.forget_terminal(&spec.key);
                return Err(RuntimeError::failed(
                    format!("starting {}", spec.name()),
                    if gone {
                        "the start command exited without the container \
                         coming up"
                    } else {
                        "the container did not come up in time"
                    },
                ));
            }

            tokio::time::sleep(wait).await;
            wait = (wait * 2).min(TERMINAL_START_POLL_MAX);
        }
    }

    fn create_args(
        &self,
        spec: &ServiceSpec,
        network: Option<&str>,
        peer_addresses: &BTreeMap<String, Ipv4Addr>,
        gateway: Option<Ipv4Addr>,
        throwaway: Option<&Throwaway<'_>>,
    ) -> Result<Vec<String>> {
        let name = match throwaway {
            Some(one_off) => one_off.name.clone(),
            None => names::container(&spec.key),
        };

        // A throwaway is run rather than created, and removed the moment
        // it exits. Nothing here should outlive the command.
        let mut args: Vec<String> = match throwaway {
            Some(_) => vec!["run".into(), "--rm".into(), "--name".into(), name.clone()],
            None => vec!["create".into(), "--name".into(), name.clone()],
        };

        args.push("--workdir".into());
        args.push(
            throwaway
                .and_then(|one_off| one_off.workdir.map(str::to_string))
                .unwrap_or_else(|| spec.workdir.clone()),
        );

        // A terminal, and a stdin that stays open for whoever attaches to
        // it later. Never for a throwaway, for the reasons the Docker
        // backend gives where it decides the same thing.
        if throwaway.is_none() && spec.tty {
            args.push("--tty".into());
            args.push("--interactive".into());
        }

        for (key, value) in self.env_with_peers(spec, peer_addresses) {
            args.push("--env".into());
            args.push(format!("{key}={value}"));
        }

        // **A throwaway carries no labels.** They are how Minato finds its
        // own containers, so a labelled one would turn up in
        // `minato status` and in what `down` stops.
        if throwaway.is_none() {
            for (key, value) in container_labels(spec) {
                args.push("--label".into());
                args.push(format!("{key}={value}"));
            }
        }

        if let Some(network) = network {
            args.push("--network".into());
            args.push(network.to_string());
        }

        // **A throwaway gets the hostnames too.** `minato exec` is where a
        // service is poked at by hand, and a curl that works in the service
        // but not in the shell beside it is the confusing kind of
        // difference.
        //
        // Nothing to point anywhere means the image's own `/etc/hosts` is
        // left alone: a file with only `localhost` in it would be a
        // downgrade, not a no-op.
        if let (Some(gateway), false) = (gateway, spec.gateway_hosts.is_empty()) {
            let hosts = self.write_hosts_file(spec, gateway)?;
            args.push("--volume".into());
            args.push(format!("{}:/etc/hosts", hosts.display()));
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
                    scope,
                } => {
                    let path = self.ensure_volume_dir(&spec.key.workspace, name, *scope)?;
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

        match throwaway {
            Some(one_off) => args.extend(one_off.command.iter().cloned()),
            None => {
                if let Some(command) = &spec.command {
                    args.extend(command.iter().cloned());
                }
            }
        }

        Ok(args)
    }
}

/// Runs a `cmd:` health check inside a container.
struct AppleCommandProbe {
    program: String,
    container: String,
}

#[async_trait]
impl crate::health::CommandProbe for AppleCommandProbe {
    async fn succeeds(&self, command: &[String]) -> bool {
        Command::new(&self.program)
            .arg("exec")
            .arg(&self.container)
            .args(command)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .is_ok_and(|status| status.success())
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
    if spec.tty {
        map.insert(labels::TTY.to_string(), labels::MANAGED_VALUE.to_string());
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

    async fn prepare(&self, spec: &WorkspaceSpec, rebuild: bool, events: &EventSink) -> Result<()> {
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
            if let Some(build) = &service.build {
                self.ensure_built(build, rebuild, events).await?;
                continue;
            }

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
            // A built image is tagged with a fingerprint of its inputs, so
            // an edited Dockerfile produces a new tag. Leaving the old
            // container up would build the new image and serve the old one.
            let wrong_image = existing
                .configuration
                .image
                .as_ref()
                .and_then(|image| image.reference.as_deref())
                .is_some_and(|reference| reference != spec.image);

            // Whether a container has a terminal is settled when it is
            // created, so `tty` turned on in `minato.toml` reaches a
            // running service only by recreating it. The label is stamped
            // at creation and came back with the listing.
            let wrong_terminal =
                (existing.label(labels::TTY) == Some(labels::MANAGED_VALUE)) != spec.tty;

            let stale = wrong_image || wrong_terminal;

            if !stale && existing.is_running() {
                events.step_skipped(
                    "start",
                    format!("starting {}", spec.name()),
                    "already running",
                );

                // No state emitted. Nothing was waited on here, so there is
                // nothing to claim: the caller settles readiness against
                // `health` before showing anyone an answer, and asserting
                // `ready` from this path would contradict it.

                return Ok(RunningService {
                    key: spec.key.clone(),
                    container_id: existing.configuration.id.clone(),
                    endpoint: existing.endpoint(spec.port),
                });
            }

            // A stopped container may be carrying a stale configuration,
            // and a running one may be on a superseded image. Either way,
            // recreate.
            self.run(&["delete", "--force", &name]).await?;
        }

        events.step_started("start", format!("starting {}", spec.name()));
        events.service_state(spec.name(), ServiceState::Starting);

        let network = self.ensure_network(&spec.attached_to, events).await?;
        // Read after the dependencies have started, so their addresses
        // exist to inject.
        let peer_addresses = self.peer_addresses(spec).await?;
        let gateway = self.network_gateway().await;
        let args = self.create_args(spec, network.as_deref(), &peer_addresses, gateway, None)?;
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();

        if let Err(err) = self.run(&arg_refs).await {
            events.step_failed(
                "start",
                format!("starting {}", spec.name()),
                err.to_string(),
            );
            return Err(err);
        }

        let started = if spec.tty {
            self.start_on_a_terminal(spec, &name).await
        } else {
            self.run(&["start", &name]).await.map(|_| ())
        };

        if let Err(err) = started {
            events.step_failed(
                "start",
                format!("starting {}", spec.name()),
                err.to_string(),
            );
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
        let probe = AppleCommandProbe {
            program: self.program.clone(),
            container: name.clone(),
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
            container_id: name,
            endpoint,
        })
    }

    async fn stop(&self, key: &ServiceKey, events: &EventSink) -> Result<()> {
        let Some(record) = self.find_container(key).await? else {
            events.step_skipped("stop", format!("stopping {}", key.service), "not running");
            return Ok(());
        };

        if !record.is_running() {
            events.step_skipped("stop", format!("stopping {}", key.service), "not running");
            return Ok(());
        }

        events.step_started("stop", format!("stopping {}", key.service));
        self.run(&["stop", &names::container(key)]).await?;

        // The terminal belonged to the container that has just gone. The
        // next start opens another one.
        self.forget_terminal(key);

        events.step_done("stop", format!("stopping {}", key.service));
        events.service_state(&key.service, ServiceState::Stopped);
        Ok(())
    }

    async fn remove(&self, key: &ServiceKey, events: &EventSink) -> Result<()> {
        self.forget_terminal(key);

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

        // Every terminal in the workspace went with its container.
        self.terminals
            .lock()
            .expect("lock")
            .retain(|service, _| &service.workspace != key);

        if self.supports_networks().await {
            let network = names::network(key);
            if !self.succeeds(&["network", "delete", &network]).await {
                events.debug(format!("network {network} was not removed"));
            }
        }

        self.remove_workspace_volumes(key, events);
        self.remove_workspace_hosts_files(key, events);

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
            .map_err(|err| {
                RuntimeError::failed(format!("reading logs for {}", key.service), err)
            })?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();

        if let Some(stdout) = stdout {
            let sender = sender.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if sender
                        .send(LogLine::new(OutputStream::Stdout, line))
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
                        .send(LogLine::new(OutputStream::Stderr, line))
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

    async fn attach(&self, key: &ServiceKey) -> Result<Attachment> {
        let (output, keyboard) = {
            let terminals = self.terminals.lock().expect("lock");
            let terminal = terminals
                .get(key)
                .filter(|terminal| terminal.is_open())
                .ok_or_else(|| {
                    RuntimeError::failed(
                        format!("attaching to {}", key.service),
                        "there is no open terminal for this service. Apple \
                         Container hands one out only to whoever starts a \
                         container, so a service that gained `tty` after it \
                         started — or one started by a daemon that has since \
                         restarted — needs `minato down && minato up` first",
                    )
                })?;

            (terminal.subscribe(), terminal.keyboard())
        };

        // A subscriber that falls too far behind is told how much it
        // missed rather than being disconnected. Dropping the gap and
        // carrying on is right for a terminal: the next redraw repairs the
        // screen, and there is nothing to be done about bytes that have
        // already been discarded.
        let output = tokio_stream::wrappers::BroadcastStream::new(output)
            .filter_map(|chunk| async move { chunk.ok() });

        Ok(Attachment {
            output: Box::pin(output),
            input: Box::pin(keyboard),
            // Measured, not assumed: a resize on this side reaches
            // `container start` and goes no further, so the program inside
            // keeps the size the terminal was opened with.
            sizing: Sizing::Fixed(crate::runtime::DEFAULT_WINDOW),
        })
    }

    async fn exec(
        &self,
        key: &ServiceKey,
        command: &[String],
        options: &ExecOptions,
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
        let mut args: Vec<String> = vec!["exec".into()];

        if let Some(workdir) = &options.workdir {
            args.push("--workdir".into());
            args.push(workdir.clone());
        }

        args.push(name);
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

    async fn exec_fresh(
        &self,
        spec: &ServiceSpec,
        command: &[String],
        options: &ExecOptions,
        events: &EventSink,
    ) -> Result<ExecOutcome> {
        // The caller's workspace, as `start` uses: `key.workspace` is the
        // reserved shared key for a `scope = "project"` service.
        let network = self.ensure_network(&spec.attached_to, events).await?;
        let peer_addresses = self.peer_addresses(spec).await?;
        let one_off = Throwaway::new(spec, command, options.workdir.as_deref());
        let gateway = self.network_gateway().await;

        let args = self.create_args(
            spec,
            network.as_deref(),
            &peer_addresses,
            gateway,
            Some(&one_off),
        )?;
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();

        // `run` blocks until the command exits and `--rm` takes the
        // container away with it, so there is nothing to clean up here.
        let output = Command::new(&self.program)
            .args(&arg_refs)
            .output()
            .await
            .map_err(|err| RuntimeError::failed("running the throwaway container", err))?;

        for line in String::from_utf8_lossy(&output.stdout).lines() {
            events.output(Some(spec.key.service.clone()), OutputStream::Stdout, line);
        }
        for line in String::from_utf8_lossy(&output.stderr).lines() {
            events.output(Some(spec.key.service.clone()), OutputStream::Stderr, line);
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
// Shaped after what `container ls --all --format json` really prints, as of
// Apple Container 1.2.1:
//
// [{ "id": "minato-myapp-feat-1-web",
//    "status": { "state": "running",
//                "networks": [{"ipv4Address": "192.168.64.3/24",
//                              "hostname": "...", "network": "default"}] },
//    "configuration": {"id": "...", "labels": {...}, "image": {...}} }]
//
// The fixture in the tests is captured from a real container rather than
// written by hand. M0 built this against a shape from the CLI's own
// documentation — `status` a bare string, `networks` alongside it — which
// the CLI does not emit, and every call failed to deserialise. A fixture
// nobody has seen the CLI produce is worth nothing.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct AppleContainerRecord {
    /// The container name. Also in `configuration.id`.
    #[serde(default)]
    pub id: Option<String>,
    /// Runtime state. **An object, not a string.**
    #[serde(default)]
    pub status: AppleStatus,
    pub configuration: AppleConfiguration,
}

/// The `status` object: what the container is doing right now.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AppleStatus {
    /// `running`, `stopped`, and so on.
    #[serde(default)]
    pub state: Option<String>,
    /// The networks it is attached to, with the addresses assigned.
    ///
    /// Under `status` rather than beside it, because an address only
    /// exists while the container is up.
    #[serde(default)]
    pub networks: Vec<AppleNetworkAttachment>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppleNetworkAttachment {
    /// Comes back with the CIDR attached (`192.168.64.3/24`).
    #[serde(default, rename = "ipv4Address")]
    pub ipv4_address: Option<String>,
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
        self.status.state.as_deref() == Some("running")
    }

    pub fn label(&self, key: &str) -> Option<&str> {
        self.configuration.labels.get(key).map(String::as_str)
    }

    /// The IPv4 address assigned on the first network.
    pub fn ip(&self) -> Option<Ipv4Addr> {
        self.status
            .networks
            .iter()
            .find_map(|net| net.ipv4_address.as_deref())
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

    /// **No `Failed` here, unlike Docker.** The Apple CLI reports a state and
    /// nothing else — there is no exit code to read — so a container that
    /// died is indistinguishable from one that was stopped. Not an
    /// oversight; there is nothing to tell them apart with.
    pub fn state(&self) -> ServiceState {
        match self.status.state.as_deref() {
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

/// The tags `container image list --format json` reports.
///
/// The tag lives at `configuration.name`, complete with registry and tag —
/// `docker.io/library/busybox:latest`, or `minato-bldapp-web:79ef7f8c89e1`
/// for something built here. Captured from the real command rather than
/// guessed: the first attempt looked for a top-level `reference`, found
/// nothing, and quietly rebuilt on every `up`.
fn parse_image_tags(json: &str) -> Result<Vec<String>> {
    if json.trim().is_empty() {
        return Ok(Vec::new());
    }

    #[derive(Deserialize)]
    struct ImageRecord {
        #[serde(default)]
        configuration: ImageConfiguration,
    }

    #[derive(Default, Deserialize)]
    struct ImageConfiguration {
        #[serde(default)]
        name: Option<String>,
    }

    let records: Vec<ImageRecord> =
        serde_json::from_str(json).map_err(|err| RuntimeError::Failed {
            operation: "parsing the output of container image list".to_string(),
            message: format!("{err}\noutput: {json}"),
        })?;

    Ok(records
        .into_iter()
        .filter_map(|record| record.configuration.name)
        .collect())
}

fn parse_network_names(json: &str) -> Result<Vec<String>> {
    if json.trim().is_empty() {
        return Ok(Vec::new());
    }

    // The name is in `configuration.name`; `id` at the top level mirrors
    // it. Reading both means a rename of either does not break lookups.
    #[derive(Deserialize)]
    struct NetworkRecord {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        configuration: NetworkConfiguration,
    }

    #[derive(Default, Deserialize)]
    struct NetworkConfiguration {
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
        .filter_map(|record| record.configuration.name.or(record.id))
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

    /// Captured from a real container on Apple Container 1.2.1.
    ///
    /// Not written by hand and not taken from the CLI's documentation:
    /// M0's fixture came from the docs, described a shape the CLI does not
    /// emit, and every call against a real machine failed to deserialise
    /// while the tests stayed green.
    const SAMPLE: &str = r#"[
      {
        "id": "minato-myapp-feat-1-web",
        "status": {
          "networks": [
            {
              "hostname": "minato-myapp-feat-1-web",
              "ipv4Address": "192.168.64.3/24",
              "ipv4Gateway": "192.168.64.1",
              "ipv6Address": "fd8f:b27a:cf00:5842:f08f:d2ff:fe1b:efa2/64",
              "macAddress": "f2:8f:d2:1b:ef:a2",
              "mtu": 1280,
              "network": "default",
              "variant": "reserved"
            }
          ],
          "startedDate": "2026-08-08T03:45:19Z",
          "state": "running"
        },
        "configuration": {
          "id": "minato-myapp-feat-1-web",
          "labels": {
            "dev.minato.managed": "1",
            "dev.minato.port": "5678",
            "dev.minato.project": "myapp",
            "dev.minato.scope": "workspace",
            "dev.minato.service": "web",
            "dev.minato.workspace": "feat-1"
          },
          "image": { "reference": "docker.io/hashicorp/http-echo:latest" }
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
            record.endpoint(Some(5678)),
            Some(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(192, 168, 64, 3)),
                5678
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
            Some(5678),
            "the port must survive a daemon restart"
        );
    }

    #[test]
    fn stopped_container_has_no_endpoint() {
        let json = SAMPLE.replace(r#""state": "running""#, r#""state": "stopped""#);
        let record = &parse_container_list(&json).expect("parses")[0];

        assert_eq!(record.state(), ServiceState::Stopped);
        assert_eq!(
            record.endpoint(Some(5678)),
            None,
            "a stopped container has no usable IP"
        );
    }

    #[test]
    fn converts_to_service_status() {
        let record = &parse_container_list(SAMPLE).expect("parses")[0];
        let status = record.to_status();

        assert_eq!(status.key.workspace.project, "myapp");
        assert_eq!(status.key.workspace.workspace, "feat-1");
        assert_eq!(status.key.service, "web");
        assert_eq!(status.state, ServiceState::Ready);
        assert_eq!(status.port, Some(5678));
        assert_eq!(
            status.image.as_deref(),
            Some("docker.io/hashicorp/http-echo:latest")
        );
        assert_eq!(status.scope, ServiceScope::Workspace);
    }

    #[test]
    fn a_running_container_reports_the_state_under_status() {
        // M0 read `status` as a bare string. The CLI emits an object, so
        // every record failed to deserialise and the runtime was unusable
        // while these tests passed against a fixture from the docs.
        let record = &parse_container_list(SAMPLE).expect("parses")[0];

        assert!(record.is_running());
        assert_eq!(record.state(), ServiceState::Ready);
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
    fn finds_a_built_image_by_tag() {
        // Captured from `container image list --format json` on 1.2.1. The
        // tag is under `configuration.name`; looking for a top-level
        // `reference` found nothing and rebuilt on every up.
        let json = r#"[
          {"id": "abc",
           "configuration": {"name": "docker.io/library/busybox:latest"}},
          {"id": "def",
           "configuration": {"name": "minato-bldapp-web:79ef7f8c89e1"}}
        ]"#;

        let tags = parse_image_tags(json).expect("parses");

        assert!(tags.iter().any(|t| t == "minato-bldapp-web:79ef7f8c89e1"));
        assert_eq!(tags.len(), 2);
    }

    #[test]
    fn an_empty_image_list_is_not_an_error() {
        assert!(parse_image_tags("[]").expect("parses").is_empty());
        assert!(parse_image_tags("").expect("empty is fine").is_empty());
    }

    #[test]
    fn parses_network_list() {
        // The shape `container network list --format json` really prints:
        // the name lives under `configuration`, mirrored by a top-level id.
        let json = r#"[
          {"id": "default",
           "configuration": {"name": "default", "mode": "nat"},
           "status": {"ipv4Subnet": "192.168.64.0/24"}},
          {"id": "minato-myapp-feat-1",
           "configuration": {"name": "minato-myapp-feat-1"}}
        ]"#;

        let names = parse_network_names(json).expect("parses");
        assert_eq!(names, vec!["default", "minato-myapp-feat-1"]);
    }

    fn spec_with_peers(peers: Vec<String>) -> ServiceSpec {
        ServiceSpec {
            key: WorkspaceKey::new("myapp", "feat-1").service("api"),
            attached_to: WorkspaceKey::new("myapp", "feat-1"),
            build: None,
            image: "node:22".into(),
            command: None,
            workdir: "/workspace".into(),
            env: BTreeMap::new(),
            tty: false,
            port: Some(8080),
            health: None,
            scope: ServiceScope::Workspace,
            volumes: vec![],
            source_mount: None,
            peers,
            gateway_hosts: vec![],
        }
    }

    #[test]
    fn maps_the_service_urls_to_the_network_gateway() {
        // There is no `--add-host` here, so the file the flag would have
        // written is mounted instead.
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime =
            AppleContainerRuntime::with_settings(PROGRAM.into(), dir.path().to_path_buf());

        let mut spec = spec_with_peers(vec![]);
        spec.gateway_hosts = vec!["web.myapp.localhost".into(), "api.myapp.localhost".into()];

        let args = runtime
            .create_args(
                &spec,
                None,
                &BTreeMap::new(),
                Some(Ipv4Addr::new(192, 168, 64, 1)),
                None,
            )
            .expect("builds the arguments");

        let mounted = args
            .windows(2)
            .find(|pair| pair[0] == "--volume" && pair[1].ends_with(":/etc/hosts"))
            .expect("mounts an /etc/hosts");

        let path = mounted[1]
            .strip_suffix(":/etc/hosts")
            .expect("checked above");
        let contents = std::fs::read_to_string(path).expect("was written");

        assert!(
            contents.contains("192.168.64.1\tweb.myapp.localhost"),
            "{contents}"
        );
        assert!(
            contents.contains("192.168.64.1\tapi.myapp.localhost"),
            "{contents}"
        );
        assert!(
            contents.contains("127.0.0.1\tlocalhost"),
            "the mount replaces the image's file, so localhost has to be \
             written back: {contents}"
        );
    }

    #[test]
    fn a_destroyed_worktree_takes_its_hosts_files_with_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime =
            AppleContainerRuntime::with_settings(PROGRAM.into(), dir.path().to_path_buf());

        let mut spec = spec_with_peers(vec![]);
        spec.gateway_hosts = vec!["api.feat-1.myapp.localhost".into()];

        let mine = runtime
            .write_hosts_file(&spec, Ipv4Addr::LOCALHOST)
            .expect("writes");

        let mut neighbour_spec = spec.clone();
        neighbour_spec.key = WorkspaceKey::new("myapp", "feat-2").service("api");
        let neighbour = runtime
            .write_hosts_file(&neighbour_spec, Ipv4Addr::LOCALHOST)
            .expect("writes");

        runtime.remove_workspace_hosts_files(
            &WorkspaceKey::new("myapp", "feat-1"),
            &EventSink::discard(),
        );

        assert!(!mine.exists(), "the destroyed worktree's file stayed");
        assert!(neighbour.exists(), "another worktree's file was taken");
    }

    #[test]
    fn mounts_no_hosts_file_when_the_network_cannot_be_asked() {
        // Without a gateway there is nowhere to point the names, and a
        // half-written /etc/hosts would take the image's own with it.
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime =
            AppleContainerRuntime::with_settings(PROGRAM.into(), dir.path().to_path_buf());

        let mut spec = spec_with_peers(vec![]);
        spec.gateway_hosts = vec!["web.myapp.localhost".into()];

        let args = runtime
            .create_args(&spec, None, &BTreeMap::new(), None, None)
            .expect("builds the arguments");

        assert!(
            !args.iter().any(|arg| arg.ends_with(":/etc/hosts")),
            "{args:?}"
        );
    }

    #[test]
    fn injects_peer_hostnames() {
        // No aliases, so the peer's container name is passed in the
        // environment.
        let runtime = AppleContainerRuntime::with_settings(
            PROGRAM.into(),
            PathBuf::from("/tmp/minato-test-volumes"),
        );
        let addresses = BTreeMap::from([
            ("db".to_string(), Ipv4Addr::new(192, 168, 64, 3)),
            ("cache-store".to_string(), Ipv4Addr::new(192, 168, 64, 4)),
        ]);
        let env = runtime.env_with_peers(
            &spec_with_peers(vec!["db".into(), "cache-store".into()]),
            &addresses,
        );

        assert_eq!(
            env.get("MINATO_HOST_DB").map(String::as_str),
            Some("192.168.64.3")
        );
        assert_eq!(
            env.get("MINATO_HOST_CACHE_STORE").map(String::as_str),
            Some("192.168.64.4"),
            "a hyphen is not valid in an environment variable name"
        );
    }

    #[test]
    fn a_peer_that_is_not_running_gets_no_variable() {
        // An unset variable fails on the missing variable, which points at
        // the ordering. A name that never resolves sends someone hunting
        // for a DNS problem that does not exist.
        let runtime = AppleContainerRuntime::with_settings(
            PROGRAM.into(),
            PathBuf::from("/tmp/minato-test-volumes"),
        );

        let env = runtime.env_with_peers(&spec_with_peers(vec!["db".into()]), &BTreeMap::new());

        assert!(!env.contains_key("MINATO_HOST_DB"), "got: {env:?}");
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

        let addresses = BTreeMap::from([("db".to_string(), Ipv4Addr::new(192, 168, 64, 3))]);
        let env = runtime.env_with_peers(&spec, &addresses);

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
            .create_args(
                &spec,
                Some("minato-myapp-feat-1"),
                &BTreeMap::new(),
                None,
                None,
            )
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
    fn a_throwaway_runs_the_given_command_and_removes_itself() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime =
            AppleContainerRuntime::with_settings(PROGRAM.into(), dir.path().to_path_buf());

        let mut spec = spec_with_peers(vec![]);
        spec.command = Some(vec!["npm".into(), "run".into(), "dev".into()]);

        let command = vec!["env".to_string()];
        let one_off = Throwaway::new(&spec, &command, None);
        let args = runtime
            .create_args(&spec, None, &BTreeMap::new(), None, Some(&one_off))
            .expect("builds");

        assert_eq!(args[0], "run", "created and left behind is not throwaway");
        assert!(args.contains(&"--rm".to_string()));
        assert_eq!(args.last().expect("a command"), "env");
        assert!(
            !args.windows(3).any(|w| w == ["npm", "run", "dev"]),
            "the service's own command must not run: {args:?}"
        );
    }

    #[test]
    fn a_throwaway_carries_no_labels() {
        // Labels are how Minato finds its own containers. A labelled
        // throwaway would show up in `minato status` and in what `down`
        // stops.
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime =
            AppleContainerRuntime::with_settings(PROGRAM.into(), dir.path().to_path_buf());

        let spec = spec_with_peers(vec![]);
        let command = vec!["sh".to_string()];
        let one_off = Throwaway::new(&spec, &command, None);

        let args = runtime
            .create_args(&spec, None, &BTreeMap::new(), None, Some(&one_off))
            .expect("builds");

        assert!(!args.iter().any(|arg| arg == "--label"), "{args:?}");

        let real = runtime
            .create_args(&spec, None, &BTreeMap::new(), None, None)
            .expect("builds");
        assert!(
            real.iter().any(|arg| arg == "--label"),
            "the real one keeps them"
        );
    }

    #[test]
    fn a_throwaway_takes_the_workdir_it_was_given() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime =
            AppleContainerRuntime::with_settings(PROGRAM.into(), dir.path().to_path_buf());

        let spec = spec_with_peers(vec![]);
        let command = vec!["ls".to_string()];
        let one_off = Throwaway::new(&spec, &command, Some("/workspace/apps/api"));

        let args = runtime
            .create_args(&spec, None, &BTreeMap::new(), None, Some(&one_off))
            .expect("builds");

        let workdir = args
            .windows(2)
            .find(|w| w[0] == "--workdir")
            .map(|w| w[1].clone())
            .expect("has a workdir");
        assert_eq!(workdir, "/workspace/apps/api");
    }

    #[test]
    fn a_throwaway_does_not_take_the_service_container_name() {
        let spec = spec_with_peers(vec![]);
        let command = vec!["sh".to_string()];
        let one_off = Throwaway::new(&spec, &command, None);

        assert_ne!(one_off.name, names::container(&spec.key));
        assert!(one_off.name.starts_with("minato-tmp-"), "{}", one_off.name);
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
            scope: crate::spec::VolumeScope::Project,
        }];

        let args = runtime
            .create_args(&spec, None, &BTreeMap::new(), None, None)
            .expect("builds");
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
