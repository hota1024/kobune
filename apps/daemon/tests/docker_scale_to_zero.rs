//! Starting and stopping services, against a real Docker.
//!
//! Mostly scale-to-zero, which is where the bugs were; also the one thing
//! about `up` that cannot be seen from a unit test, which is whether two
//! services actually come up at the same time.
//!
//! **Nothing else here runs against a container runtime.** `docker.rs` is
//! most of two thousand lines with a dozen tests, all of them parsing
//! fixtures written by hand — the arrangement `docs/DESIGN.md` §6 blames
//! for M0's Apple Container backend being wholly broken while its tests
//! passed. The Docker backend still has that exposure, and the two bugs
//! this file starts with were both invisible to a unit test and both
//! obvious within a minute of running it.
//!
//! Every test here is `#[ignore]`d, so `cargo test` is untouched:
//!
//! ```console
//! $ cargo test -p minatod -- --ignored --test-threads=1
//! ```
//!
//! **One at a time.** They share a Docker daemon and name their
//! containers from the project in `minato.toml`, so two running at once
//! would tread on each other. CI passes `--test-threads=1`; so should you.
//!
//! What is deliberately *not* here is the proxy and DNS. The gateway is
//! inert and the wake is asked for directly, which is the same call the
//! proxy makes ([`minatod::activator`]) — `minato-proxy`'s own end-to-end
//! tests already cover the HTTP half against a stub. What could not be
//! covered any other way is the runtime underneath.

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
const START_WAIT: Duration = Duration::from_secs(180);

/// Long enough for a container to start or stop on a busy runner.
///
/// Generous on purpose. Every use of it is a wait for something that has
/// already been asked for, so the only thing a tight bound buys is a
/// failure that says nothing about the code.
const SETTLE_WAIT: Duration = Duration::from_secs(90);

/// Skips the test when there is no runtime to talk to.
///
/// **Reported, not silently passed.** A suite that quietly does nothing
/// is worse than no suite, because it reads as coverage.
macro_rules! require_docker {
    () => {
        if !docker_is_running() {
            eprintln!("skipped: no Docker daemon answered");
            return;
        }
    };
}

fn docker_is_running() -> bool {
    Command::new("docker")
        .args(["version", "--format", "{{.Server.Version}}"])
        .output()
        .is_ok_and(|out| out.status.success())
}

fn git(dir: &Path, args: &[&str]) {
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
struct Harness {
    _home: tempfile::TempDir,
    _repo: tempfile::TempDir,
    root: std::path::PathBuf,
    project: String,
    supervisor: Arc<Supervisor>,
}

impl Harness {
    /// `project` names the containers, so give each test its own.
    fn new(project: &str, config: &str) -> Self {
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

    fn target(&self) -> Target {
        Target::new(self.root.clone())
    }

    async fn request(&self, request: Request) -> minato_api::Response {
        self.request_watching(request, &EventSink::discard()).await
    }

    async fn request_watching(&self, request: Request, events: &EventSink) -> minato_api::Response {
        // Nothing here types at a terminal, so the keyboard channel is
        // only ever the shape the signature wants.
        let (_keys, from_client) = tokio::sync::mpsc::unbounded_channel();

        self.supervisor
            .handle(request, events, from_client)
            .await
            .unwrap_or_else(|err| panic!("{err:?}"))
    }

    fn up_request(&self) -> Request {
        Request::Up {
            target: self.target(),
            services: Vec::new(),
            rebuild: false,
        }
    }

    async fn up(&self) {
        self.request(self.up_request()).await;
    }

    /// One `up`, and everything it said while doing it.
    ///
    /// Drained rather than awaited: the sends are synchronous and the
    /// request has already returned, so everything is queued by now. A
    /// `recv` loop would instead hang on any clone of the sink the daemon
    /// happens to be holding.
    async fn up_watching(&self) -> Vec<minato_api::Event> {
        let (events, mut received) = EventSink::channel();
        self.request_watching(self.up_request(), &events).await;

        let mut all = Vec::new();
        while let Ok(event) = received.try_recv() {
            all.push(event);
        }
        all
    }

    async fn down(&self) {
        self.request(Request::Down {
            target: self.target(),
            services: Vec::new(),
            all: false,
        })
        .await;
    }

    /// The hostname a request would arrive on. The main worktree drops
    /// the workspace label, so this is `{service}.{project}.localhost`.
    fn host(&self, service: &str) -> String {
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
    async fn running(&self) -> Vec<String> {
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
    async fn wait_until_running(&self, expected: &[&str]) {
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
    async fn sweep_until_empty(&self) {
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

/// `web` is reachable and depends on `db`, which is not.
///
/// The shape `minato init` writes and the documentation recommends, and
/// the one both bugs needed.
fn web_and_db(project: &str) -> String {
    format!(
        r#"
[project]
name = "{project}"

[runtime]
default = "docker"

[services.web]
image = "busybox:latest"
port = 8000
command = "sh -c 'echo ok > /tmp/index.html; httpd -f -p 8000 -h /tmp'"
depends_on = ["db"]
idle_timeout = "1s"

[services.db]
image = "busybox:latest"
port = 5432
expose = false
command = "sh -c 'sleep infinity'"
idle_timeout = "1s"
"#
    )
}

#[tokio::test]
#[ignore = "needs a Docker daemon"]
async fn waking_a_service_brings_up_what_it_depends_on() {
    require_docker!();

    // The wake path used to start the single service the host named.
    // `up` resolved `depends_on` and it did not, so a workspace worked
    // until scale-to-zero stopped something.
    let harness = Harness::new("mnte2edeps", &web_and_db("mnte2edeps"));

    harness.up().await;
    harness.down().await;

    // **Waited for, not asserted.** `down` asks the runtime to stop and
    // a container is not stopped the instant it is asked; a busy runner
    // is where that gap shows. The same shape of assertion was what made
    // `a_swept_service_comes_back_on_the_next_request` flaky, and this
    // one was left behind when that was fixed.
    harness.wait_until_running(&[]).await;

    let activation = harness
        .supervisor
        .activate(&harness.host("web"), START_WAIT)
        .await;

    assert!(
        matches!(activation, minato_proxy::Activation::Ready(_)),
        "the request should have been answered: {activation:?}"
    );
    // A service with no URL has no request of its own to arrive, so it
    // comes up only if the wake brought it.
    harness.wait_until_running(&["db", "web"]).await;
}

#[tokio::test]
#[ignore = "needs a Docker daemon"]
async fn an_internal_service_follows_its_last_dependent_down() {
    require_docker!();

    // `route_entries` skips unexposed services and the sweep walks
    // nothing but the routing table, so `db` was never a candidate and
    // stayed up for as long as the daemon did.
    let harness = Harness::new("mnte2eidle", &web_and_db("mnte2eidle"));

    harness.up().await;
    harness.wait_until_running(&["db", "web"]).await;

    // Past the 1s timeout in the configuration above.
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Twice: the first stops `web`, and `db` follows in the same pass —
    // but only once nothing is left holding it. Sweeping again proves
    // the second one is not what did it.
    let stopped = harness.supervisor.sweep_idle().await;
    assert_eq!(stopped, 2, "web and the db behind it");

    // The count above is what pins "in the same sweep". This is only
    // the state settling afterwards, so it waits.
    harness.wait_until_running(&[]).await;
}

#[tokio::test]
#[ignore = "needs a Docker daemon"]
async fn an_internal_service_nothing_depends_on_is_left_alone() {
    require_docker!();

    // Waking follows `depends_on` outwards from a service a request can
    // reach. With nothing pointing at it there is no way back up, so
    // stopping it would be one-way.
    let project = "mnte2eorphan";
    let harness = Harness::new(
        project,
        &format!(
            r#"
[project]
name = "{project}"

[runtime]
default = "docker"

[services.web]
image = "busybox:latest"
port = 8000
command = "sh -c 'echo ok > /tmp/index.html; httpd -f -p 8000 -h /tmp'"
idle_timeout = "1s"

[services.db]
image = "busybox:latest"
port = 5432
expose = false
command = "sh -c 'sleep infinity'"
idle_timeout = "1s"
"#
        ),
    );

    harness.up().await;
    harness.wait_until_running(&["db", "web"]).await;

    tokio::time::sleep(Duration::from_secs(2)).await;
    harness.supervisor.sweep_idle().await;

    // `web` goes; `db` stays, because nothing would ever start it again.
    harness.wait_until_running(&["db"]).await;
}

#[tokio::test]
#[ignore = "needs a Docker daemon"]
async fn a_swept_service_comes_back_on_the_next_request() {
    require_docker!();

    // The whole promise: ten worktrees, and only what is in use running.
    // It only holds if the way back is as reliable as the way down.
    let harness = Harness::new("mnte2ecycle", &web_and_db("mnte2ecycle"));

    harness.up().await;
    harness.wait_until_running(&["db", "web"]).await;

    // Past the 1s timeout in the configuration above.
    tokio::time::sleep(Duration::from_secs(2)).await;
    harness.sweep_until_empty().await;

    let activation = harness
        .supervisor
        .activate(&harness.host("web"), START_WAIT)
        .await;

    assert!(
        matches!(activation, minato_proxy::Activation::Ready(_)),
        "{activation:?}"
    );
    harness.wait_until_running(&["db", "web"]).await;
}

#[tokio::test]
#[ignore = "needs a Docker daemon"]
async fn starting_twice_leaves_one_container() {
    require_docker!();

    // `up` is documented as repeatable, and the wake path now starts
    // dependencies that are very often already up — so "start something
    // already started" went from a corner to the common case.
    let harness = Harness::new("mnte2erepeat", &web_and_db("mnte2erepeat"));

    harness.up().await;
    harness.wait_until_running(&["db", "web"]).await;

    harness.up().await;
    harness
        .supervisor
        .activate(&harness.host("web"), START_WAIT)
        .await;

    // Still two, and the same two. A second copy would show up here as a
    // duplicate service name, since `running` reads the runtime's labels.
    harness.wait_until_running(&["db", "web"]).await;
}

/// Two services that know nothing of each other, each slow to answer.
///
/// The sleep is what makes the difference visible: a start is only slow
/// because of the readiness wait behind it, and with nothing to wait for
/// both orders finish at once and prove nothing.
fn two_slow_services(project: &str) -> String {
    format!(
        r#"
[project]
name = "{project}"

[runtime]
default = "docker"

[services.web]
image = "busybox:latest"
port = 8000
command = "sh -c 'sleep 4; echo ok > /tmp/index.html; httpd -f -p 8000 -h /tmp'"

[services.api]
image = "busybox:latest"
port = 8080
command = "sh -c 'sleep 4; echo ok > /tmp/index.html; httpd -f -p 8080 -h /tmp'"
"#
    )
}

/// Where the first event saying `step` reached a matching status sits.
fn step_at(
    events: &[minato_api::Event],
    step: &str,
    matching: impl Fn(&minato_api::StepStatus) -> bool,
) -> usize {
    events
        .iter()
        .position(|event| {
            matches!(
                event,
                minato_api::Event::Step { id, status, .. } if id == step && matching(status)
            )
        })
        .unwrap_or_else(|| panic!("no matching `{step}` in {events:#?}"))
}

#[tokio::test]
#[ignore = "needs a Docker daemon"]
async fn independent_services_do_not_wait_for_each_other() {
    require_docker!();

    // Read off the event stream rather than a stopwatch. Overlap is the
    // claim — that neither service's readiness wait had finished before
    // the other's began — and a wall-clock bound would only be that claim
    // measured through a busy CI runner.
    let harness = Harness::new("mnte2epar", &two_slow_services("mnte2epar"));

    let events = harness.up_watching().await;

    use minato_api::StepStatus;
    let started = |service: &str| {
        step_at(&events, &format!("await-{service}"), |status| {
            matches!(status, StepStatus::Started)
        })
    };
    // Spelled out rather than "anything but `Started`": a wait that
    // reported `Progress` would satisfy that and be read as finished.
    let settled = |service: &str| {
        step_at(&events, &format!("await-{service}"), |status| {
            matches!(
                status,
                StepStatus::Done | StepStatus::Skipped { .. } | StepStatus::Failed { .. }
            )
        })
    };

    assert!(
        started("api") < settled("web") && started("web") < settled("api"),
        "the two waits did not overlap, so one service waited out the \
         other: {events:#?}"
    );

    // The ordering above is the point; this is only that nothing fell
    // over on the way.
    harness.wait_until_running(&["api", "web"]).await;
}
