//! Building an image, against a real Docker.
//!
//! **The builder is not an implementation detail.** Docker's own `docker
//! build` has used BuildKit since Docker 23, so the Dockerfiles people
//! write assume it: `RUN --mount=type=cache`, heredocs and a `# syntax=`
//! frontend are all hard errors under the legacy builder. A unit test
//! cannot tell which builder ran — the request is the same either way, and
//! only the daemon knows the difference — so this is the only place the
//! answer is visible.
//!
//! Every test here is `#[ignore]`d, so `cargo test` is untouched:
//!
//! ```console
//! $ cargo test -p kobuned -- --ignored --test-threads=1
//! ```
//!
//! **One at a time**, for the reason `docker_scale_to_zero.rs` gives: they
//! share a Docker daemon and are told apart only by their project name.

use kobune_api::{Event, StepStatus};

#[macro_use]
mod common;

use common::Harness;

/// One service, built rather than pulled.
fn built_service(project: &str) -> String {
    format!(
        r#"
[project]
name = "{project}"

[runtime]
default = "docker"

[services.web]
build = "."
port = 8000
idle_timeout = "60s"
"#
    )
}

/// A Dockerfile using the one instruction the legacy builder refuses.
///
/// `--mount=type=cache` is answered with "the --mount option requires
/// BuildKit" by the old builder and honoured by the new one, so a build
/// that finishes at all is the assertion.
const NEEDS_BUILDKIT: &str = "\
FROM busybox:latest
RUN --mount=type=cache,target=/var/cache/probe \\
    echo ok > /tmp/index.html
CMD [\"httpd\", \"-f\", \"-p\", \"8000\", \"-h\", \"/tmp\"]
";

/// Every line the build reported, in order.
fn build_output(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Step {
                id,
                status: StepStatus::Progress { message },
                ..
            } if id.starts_with("build") => Some(message.clone()),
            _ => None,
        })
        .collect()
}

/// What the build failed with, if it did.
fn build_failure(events: &[Event]) -> Option<String> {
    events.iter().find_map(|event| match event {
        Event::Step {
            id,
            status: StepStatus::Failed { reason },
            ..
        } if id.starts_with("build") => Some(reason.clone()),
        _ => None,
    })
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Docker"]
async fn a_dockerfile_that_needs_buildkit_builds() {
    require_docker!();

    let harness = Harness::new("kobune-build-bk", &built_service("kobune-build-bk"));
    std::fs::write(harness.root.join("Dockerfile"), NEEDS_BUILDKIT).expect("writes");

    let events = harness.up_watching().await;

    assert_eq!(
        build_failure(&events),
        None,
        "the build failed; output was {:?}",
        build_output(&events)
    );

    harness.wait_until_running(&["web"]).await;
    harness.down().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Docker"]
async fn the_build_says_which_step_it_is_on() {
    require_docker!();

    let harness = Harness::new("kobune-build-say", &built_service("kobune-build-say"));
    std::fs::write(harness.root.join("Dockerfile"), NEEDS_BUILDKIT).expect("writes");

    let events = harness.up_watching().await;
    let output = build_output(&events);

    // **The number, not the text.** What BuildKit calls a step changes
    // with the version; that every line is attributed to one is the
    // contract `crate::buildkit` exists to keep.
    assert!(
        output.iter().any(|line| line.starts_with('#')),
        "no step-numbered output; got {output:?}"
    );

    harness.down().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Docker"]
async fn a_dockerignore_keeps_what_it_names_out_of_the_context() {
    require_docker!();

    // **The Docker daemon does not do this filtering**, so a unit test can
    // only say Kobune packed the right tar. Whether the build then sees
    // what it should is this.
    //
    // `*` with a `!` line is also the shape that breaks a build if the
    // Dockerfile is not put back: it names the Dockerfile along with
    // everything else, and BuildKit reads the Dockerfile out of the tar.
    let dockerfile = "\
FROM busybox:latest
COPY . /src
RUN test ! -e /src/ignored.txt
RUN test -e /src/kept.txt
RUN echo ok > /tmp/index.html
CMD [\"httpd\", \"-f\", \"-p\", \"8000\", \"-h\", \"/tmp\"]
";

    let project = "kobune-build-ignore";
    let harness = Harness::new(project, &built_service(project));
    let root = &harness.root;

    std::fs::write(root.join("Dockerfile"), dockerfile).expect("writes");
    std::fs::write(root.join(".dockerignore"), "*\n!kept.txt\n").expect("writes");
    std::fs::write(root.join("kept.txt"), "kept").expect("writes");
    std::fs::write(root.join("ignored.txt"), "ignored").expect("writes");

    let events = harness.up_watching().await;

    assert_eq!(
        build_failure(&events),
        None,
        "the build failed; output was {:?}",
        build_output(&events)
    );

    harness.wait_until_running(&["web"]).await;
    harness.down().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Docker"]
async fn a_dockerfile_from_outside_the_context_builds() {
    require_docker!();

    // **One context, several images** is the layout `dockerfile` exists
    // for, and it had never worked: the Dockerfile went into the tar after
    // the end-of-archive marker, so the daemon was sent a context without
    // one. Nothing but a real build says whether it is there.
    let project = "kobune-build-outside";
    let config = format!(
        r#"
[project]
name = "{project}"

[runtime]
default = "docker"

[services.web]
build = "./web"
dockerfile = "./docker/web.Dockerfile"
port = 8000
idle_timeout = "60s"
"#
    );

    let harness = Harness::new(project, &config);
    let root = &harness.root;

    std::fs::create_dir_all(root.join("web")).expect("creates");
    std::fs::create_dir_all(root.join("docker")).expect("creates");
    std::fs::write(root.join("web/app.txt"), "the app").expect("writes");
    std::fs::write(
        root.join("docker/web.Dockerfile"),
        "\
FROM busybox:latest
COPY app.txt /app.txt
RUN echo ok > /tmp/index.html
CMD [\"httpd\", \"-f\", \"-p\", \"8000\", \"-h\", \"/tmp\"]
",
    )
    .expect("writes");

    let events = harness.up_watching().await;

    assert_eq!(
        build_failure(&events),
        None,
        "the build failed; output was {:?}",
        build_output(&events)
    );

    harness.wait_until_running(&["web"]).await;
    harness.down().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Docker"]
async fn a_failed_build_quotes_what_the_step_printed() {
    require_docker!();

    // "exit code: 1" is not an answer. What the command printed before it
    // died is, and it has already gone past as progress.
    let dockerfile = "\
FROM busybox:latest
RUN echo the-reason-it-failed && exit 3
";

    let harness = Harness::new("kobune-build-fail", &built_service("kobune-build-fail"));
    std::fs::write(harness.root.join("Dockerfile"), dockerfile).expect("writes");

    let (outcome, events) = harness.up_watching_outcome().await;
    assert!(outcome.is_err(), "a build that exits 3 is not a success");

    let message = build_failure(&events).expect("the build reports a failure");

    assert!(
        message.contains("the-reason-it-failed"),
        "the failure did not quote the step's output: {message}"
    );
}
