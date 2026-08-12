//! What every test against a real container runtime needs.
//!
//! A project, a daemon and somewhere for both to live — extracted here
//! because the runtime tests are no longer one file, and the half of them
//! that is setup was never what any of them was about.
//!
//! **Every item is used by one test binary or another, and each compiles
//! this module separately.** Rust therefore sees whatever the file it is
//! building does not touch as dead, which under `-D warnings` would make
//! adding a helper for one suite break the other.
//!
//! **The home is temporary; Docker is not.** [`Harness`] hands the daemon a
//! `MINATO_HOME` of its own, so anything under that directory belongs to
//! the test — but the containers, networks and volumes it makes are real,
//! shared with whatever else is on the machine, and told apart only by the
//! project name. Never send this supervisor a `Purge { dry_run: false }`:
//! the storage sweep is machine-wide on purpose (see `docker_uninstall.rs`)
//! and would take the volumes of whoever is running the suite.
#![allow(dead_code)]

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use minato_api::{Request, Target};
use minato_core::Paths;
use minato_runtime::{EventSink, Runtime};
use minatod::gateway::Gateway;
use minatod::supervisor::Supervisor;
use minatod::tunnel::TunnelHandle;

/// Long enough for `busybox` to be pulled on a cold runner.
pub const START_WAIT: Duration = Duration::from_secs(180);

/// Long enough for a container to start or stop on a busy runner.
///
/// Generous on purpose. Every use of it is a wait for something that has
/// already been asked for, so the only thing a tight bound buys is a
/// failure that says nothing about the code.
pub const SETTLE_WAIT: Duration = Duration::from_secs(90);

/// Skips the test when there is no runtime to talk to.
///
/// **Reported, not silently passed.** A suite that quietly does nothing
/// is worse than no suite, because it reads as coverage.
macro_rules! require_docker {
    () => {
        if !$crate::common::docker_is_running() {
            eprintln!("skipped: no Docker daemon answered");
            return;
        }
    };
}

pub fn docker_is_running() -> bool {
    Command::new("docker")
        .args(["version", "--format", "{{.Server.Version}}"])
        .output()
        .is_ok_and(|out| out.status.success())
}

pub fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap_or_else(|err| panic!("cannot run git: {err}"));

    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A project, its daemon, and somewhere for both to live.
///
/// The runtime is the only thing here that is real. Everything else is a
/// temporary directory that goes with the test.
pub struct Harness {
    _home: tempfile::TempDir,
    _repo: tempfile::TempDir,
    pub root: std::path::PathBuf,
    pub project: String,
    pub supervisor: Arc<Supervisor>,
}

impl Harness {
    /// `project` names the containers, so give each test its own.
    pub fn new(project: &str, config: &str) -> Self {
        let home = tempfile::tempdir().expect("tempdir");
        let repo = tempfile::tempdir().expect("tempdir");

        // A tempdir reaches through a symlink on macOS (/var → /private/var)
        // and git reports the real path, so compare like with like.
        let root = repo.path().canonicalize().expect("the path exists");

        git(&root, &["init", "--initial-branch=main"]);
        git(&root, &["config", "user.email", "test@example.com"]);
        git(&root, &["config", "user.name", "Minato Test"]);
        git(&root, &["config", "commit.gpgsign", "false"]);
        std::fs::write(root.join("minato.toml"), config).expect("writes");
        git(&root, &["add", "."]);
        git(&root, &["commit", "-m", "initial"]);

        let paths = Paths::with_root(home.path().to_path_buf());
        paths.ensure().expect("creates the home directory");

        let supervisor = Arc::new(Supervisor::new(
            &paths,
            Arc::new(Gateway::inert()),
            TunnelHandle::new(),
            Arc::new(tokio::sync::Notify::new()),
        ));

        Self {
            _home: home,
            _repo: repo,
            root,
            project: project.to_string(),
            supervisor,
        }
    }

    pub fn target(&self) -> Target {
        Target::new(self.root.clone())
    }

    pub async fn request(&self, request: Request) -> minato_api::Response {
        self.request_watching(request, &EventSink::discard()).await
    }

    pub async fn request_watching(
        &self,
        request: Request,
        events: &EventSink,
    ) -> minato_api::Response {
        // Nothing here types at a terminal, so the keyboard channel is
        // only ever the shape the signature wants.
        let (_keys, from_client) = tokio::sync::mpsc::unbounded_channel();

        self.supervisor
            .handle(request, events, from_client)
            .await
            .unwrap_or_else(|err| panic!("{err:?}"))
    }

    pub fn up_request(&self) -> Request {
        Request::Up {
            target: self.target(),
            services: Vec::new(),
            rebuild: false,
        }
    }

    pub async fn up(&self) {
        self.request(self.up_request()).await;
    }

    /// One `up`, and everything it said while doing it.
    ///
    /// Drained rather than awaited: the sends are synchronous and the
    /// request has already returned, so everything is queued by now. A
    /// `recv` loop would instead hang on any clone of the sink the daemon
    /// happens to be holding.
    pub async fn up_watching(&self) -> Vec<minato_api::Event> {
        let (events, mut received) = EventSink::channel();
        self.request_watching(self.up_request(), &events).await;

        let mut all = Vec::new();
        while let Ok(event) = received.try_recv() {
            all.push(event);
        }
        all
    }

    pub async fn down(&self) {
        self.request(Request::Down {
            target: self.target(),
            services: Vec::new(),
            all: false,
        })
        .await;
    }

    /// The hostname a request would arrive on. The main worktree drops
    /// the workspace label, so this is `{service}.{project}.localhost`.
    pub fn host(&self, service: &str) -> String {
        format!("{service}.{}.localhost", self.project)
    }

    /// Which of the project's services the runtime says are up, right
    /// now.
    ///
    /// **A snapshot of an asynchronous system, so not a thing to assert
    /// on.** `up` and `down` return when the runtime has been asked; a
    /// container is neither running nor stopped the instant it is asked,
    /// and a loaded runner is where that gap becomes visible. Build a
    /// wait out of this — [`Harness::wait_until_running`] — rather than
    /// comparing it to what you expect. Three unrelated pull requests
    /// were held up by assertions that did the latter.
    pub async fn running(&self) -> Vec<String> {
        let runtime = minato_runtime::docker::DockerRuntime::connect().expect("Docker answers");

        let mut names: Vec<String> = runtime
            .list_project(&self.project)
            .await
            .expect("lists")
            .into_iter()
            .filter(|status| status.state.is_running())
            .map(|status| status.key.service)
            .collect();

        names.sort();
        names
    }

    /// Waits until the project is running exactly `expected`.
    ///
    /// **The precondition, established rather than assumed.** A test that
    /// starts its clock believing everything is up reports the wrong
    /// thing when it is not, and CI is where that happens.
    ///
    /// Nothing is swept here. These services have an `idle_timeout` of a
    /// second, so a sweep inside a wait for a *start* would take away
    /// what the wait is waiting for.
    pub async fn wait_until_running(&self, expected: &[&str]) {
        let deadline = std::time::Instant::now() + SETTLE_WAIT;

        loop {
            let running = self.running().await;
            if running == expected {
                return;
            }

            assert!(
                std::time::Instant::now() < deadline,
                "waited {}s for {expected:?} to be up; running: {running:?}",
                SETTLE_WAIT.as_secs()
            );

            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// Sweeps until nothing is left, and says what survived if that never
    /// happens.
    ///
    /// **Not a single sweep.** That one pass takes a service and the
    /// database behind it is a real guarantee, and
    /// `an_internal_service_follows_its_last_dependent_down` is where it
    /// is pinned — deterministically, on a count rather than on the state
    /// of a container afterwards. Asserting it again here only gives the
    /// round trip a second way to fail for somebody else's reason.
    pub async fn sweep_until_empty(&self) {
        let deadline = std::time::Instant::now() + SETTLE_WAIT;

        loop {
            self.supervisor.sweep_idle().await;

            let running = self.running().await;
            if running.is_empty() {
                return;
            }

            assert!(
                std::time::Instant::now() < deadline,
                "waited {}s for the sweep to take everything; still running: {running:?}",
                SETTLE_WAIT.as_secs()
            );

            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
}

/// Leaves nothing of this project behind, **whatever the test did**.
///
/// In `Drop` rather than at the end of each test, because the run that
/// most needs cleaning up after is the one that panicked half-way. And
/// through the `docker` CLI rather than the runtime under test: a
/// cleanup that fails for the same reason the test did is no cleanup,
/// and a container left behind is not only litter — the next run would
/// find it and draw the wrong conclusion.
impl Drop for Harness {
    fn drop(&mut self) {
        let filter = format!("label={}={}", minato_runtime::labels::PROJECT, self.project);

        let ids = Command::new("docker")
            .args(["ps", "-aq", "--filter", &filter])
            .output();

        if let Ok(ids) = ids {
            let ids: Vec<String> = String::from_utf8_lossy(&ids.stdout)
                .split_whitespace()
                .map(str::to_string)
                .collect();

            if !ids.is_empty() {
                let _ = Command::new("docker")
                    .arg("rm")
                    .arg("-f")
                    .args(&ids)
                    .output();
            }
        }

        for kind in ["volume", "network"] {
            if let Ok(listed) = Command::new("docker")
                .args([kind, "ls", "-q", "--filter", &filter])
                .output()
            {
                for name in String::from_utf8_lossy(&listed.stdout).split_whitespace() {
                    let _ = Command::new("docker").args([kind, "rm", name]).output();
                }
            }
        }
    }
}
