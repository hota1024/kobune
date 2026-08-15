//! What a full-screen program made of its terminal, heard from a real
//! container.
//!
//! **This is the shape a unit test could not have.** Every unit test around
//! `Modes` hands it bytes directly, so they check the parser and never the
//! delivery — and the delivery was the whole of the bug. The feature was
//! built to read a container's log back and find the announcement in it,
//! which cannot work: Docker holds back everything after the last newline
//! until the process ends, and a program that draws by moving the cursor
//! has no reason ever to end a line. Nothing on this side of the socket
//! could have found those bytes, because they were never sent.
//!
//! So the daemon watches the terminal instead, from before the container
//! starts. That is a fact about a running Docker and cannot be faked.
//!
//! Ignored like the rest of the suites that need a runtime:
//!
//! ```console
//! $ cargo test -p kobuned --test docker_terminal_modes -- --ignored --test-threads=1
//! ```

use std::time::Duration;

#[macro_use]
mod common;

use common::Harness;
use futures::StreamExt;

/// A service that announces itself the way a full-screen program does.
///
/// **The missing newline is the whole test.** One line that ends in `\n`,
/// then the announcement, and then nothing ever again — which is what a
/// program that positions the cursor to draw looks like from outside. Ask
/// this container for its log while it runs and Docker answers `starting`
/// and stops there; ask again after it exits and the rest appears. The
/// announcement is only ever heard by someone already listening.
fn full_screen(project: &str) -> String {
    format!(
        r#"
[project]
name = "{project}"

[runtime]
default = "docker"

[services.tui]
image = "busybox:latest"
port = 8000
tty = true
command = '''sh -c 'printf "starting\n"; printf "\033[?1049h\033[?1000h"; sleep infinity' '''
"#
    )
}

#[tokio::test]
#[ignore = "needs a Docker daemon"]
async fn a_full_screen_program_is_heard_although_its_log_never_says() {
    require_docker!();

    let harness = Harness::new("mnte2emodes", &full_screen("mnte2emodes"));
    harness.up().await;
    harness.wait_until_running(&["tui"]).await;

    // The announcement is written straight after start, but "started" and
    // "has written" are not the same instant.
    tokio::time::sleep(Duration::from_secs(2)).await;

    // **The daemon's own instance, not a fresh connection to the same
    // Docker.** What the program made of its terminal is watched as it
    // goes past and held by the runtime that started the container, so a
    // second instance would report an empty preamble and look exactly like
    // the failure this test is here for.
    let runtime = harness
        .supervisor
        .runtime("docker")
        .await
        .expect("the daemon's Docker runtime");

    // Taken from the runtime rather than built here: the workspace is named
    // after the branch, and a key assembled from a guess about that would
    // fail as "no such service" and look like the bug this is about.
    let key = runtime
        .list_project("mnte2emodes")
        .await
        .expect("lists the project")
        .into_iter()
        .map(|status| status.key)
        .find(|key| key.service == "tui")
        .expect("the service is there");

    let attachment = runtime.attach(&key).await.expect("attaches");

    // The preamble is not a field: it is prepended to the stream, so this
    // asserts on what a client is actually handed. One chunk is enough —
    // the modes lead it — but the service is alive and quiet, so the read
    // needs a clock or it waits for output that is never coming.
    let mut output = attachment.output;
    let first = tokio::time::timeout(Duration::from_secs(10), output.next())
        .await
        .expect("the attachment produced something")
        .expect("the stream had a chunk");

    let seen = String::from_utf8_lossy(&first).into_owned();

    assert!(
        seen.contains("\x1b[?1049h"),
        "the alternate screen was announced and not heard: {seen:?}"
    );
    assert!(
        seen.contains("\x1b[?1000h"),
        "the mouse was asked for and not heard: {seen:?}"
    );

    harness.down().await;
}
