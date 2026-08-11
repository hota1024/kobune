//! Scale-to-zero, against a real Docker.
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
        // Nothing here types at a terminal, so the keyboard channel is
        // only ever the shape the signature wants.
        let (_keys, from_client) = tokio::sync::mpsc::unbounded_channel();

        self.supervisor
            .handle(request, &EventSink::discard(), from_client)
            .await
            .unwrap_or_else(|err| panic!("{err:?}"))
    }

    async fn up(&self) {
        self.request(Request::Up {
            target: self.target(),
            services: Vec::new(),
            rebuild: false,
        })
        .await;
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

    /// Which of the project's services the runtime says are up.
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
    assert!(harness.running().await.is_empty(), "nothing is up to start");

    let activation = harness
        .supervisor
        .activate(&harness.host("web"), START_WAIT)
        .await;

    assert!(
        matches!(activation, minato_proxy::Activation::Ready(_)),
        "the request should have been answered: {activation:?}"
    );
    assert_eq!(
        harness.running().await,
        vec!["db".to_string(), "web".to_string()],
        "a service with no URL has no request of its own to arrive"
    );
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
    assert_eq!(
        harness.running().await,
        vec!["db".to_string(), "web".to_string()]
    );

    // Past the 1s timeout in the configuration above.
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Twice: the first stops `web`, and `db` follows in the same pass —
    // but only once nothing is left holding it. Sweeping again proves
    // the second one is not what did it.
    let stopped = harness.supervisor.sweep_idle().await;
    assert_eq!(stopped, 2, "web and the db behind it");

    assert!(
        harness.running().await.is_empty(),
        "a database per worktree, running for ever, is what this exists to prevent"
    );
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
    tokio::time::sleep(Duration::from_secs(2)).await;
    harness.supervisor.sweep_idle().await;

    assert_eq!(
        harness.running().await,
        vec!["db".to_string()],
        "nothing would ever start it again"
    );
}

#[tokio::test]
#[ignore = "needs a Docker daemon"]
async fn a_swept_service_comes_back_on_the_next_request() {
    require_docker!();

    // The whole promise: ten worktrees, and only what is in use running.
    // It only holds if the way back is as reliable as the way down.
    let harness = Harness::new("mnte2ecycle", &web_and_db("mnte2ecycle"));

    harness.up().await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    harness.supervisor.sweep_idle().await;
    assert!(harness.running().await.is_empty(), "swept");

    let activation = harness
        .supervisor
        .activate(&harness.host("web"), START_WAIT)
        .await;

    assert!(
        matches!(activation, minato_proxy::Activation::Ready(_)),
        "{activation:?}"
    );
    assert_eq!(
        harness.running().await,
        vec!["db".to_string(), "web".to_string()]
    );
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
    let first = harness.running().await;

    harness.up().await;
    harness
        .supervisor
        .activate(&harness.host("web"), START_WAIT)
        .await;

    assert_eq!(harness.running().await, first, "no second copy of anything");
}
