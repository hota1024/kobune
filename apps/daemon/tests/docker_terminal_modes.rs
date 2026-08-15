//! What a full-screen program left on its terminal, read back from a real
//! container.
//!
//! **This is the shape a unit test could not have.** The scan reads a
//! container's log through Docker's API, and what it gets back depends on
//! how the client library frames that stream — which no fixture written by
//! hand describes. Reading the log through `logs` returned nothing at all
//! for the programs the feature exists for, and every unit test around it
//! passed the whole time: they hand `Modes::watch` the bytes directly, so
//! they check the parser and never the delivery.
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
use kobune_runtime::{DockerRuntime, Runtime};

/// A service that announces itself the way a full-screen program does.
///
/// **The newline placement is the whole test.** `printf` writes a line that
/// ends in `\n`, then the announcement, and then nothing ever again — which
/// is what a program that draws by positioning the cursor looks like. Read
/// through a decoder that splits on `\n`, everything from `ESC[?1049h`
/// onwards stays buffered and is dropped at the end of the stream, so the
/// scan sees the first line and none of what it came for.
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
async fn a_full_screen_program_is_heard_through_the_last_newline() {
    require_docker!();

    let harness = Harness::new("mnte2emodes", &full_screen("mnte2emodes"));
    harness.up().await;
    harness.wait_until_running(&["tui"]).await;

    // The announcement is written straight after start, but "started" and
    // "has written" are not the same instant.
    tokio::time::sleep(Duration::from_secs(2)).await;

    let runtime = DockerRuntime::connect().expect("connects to Docker");

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
