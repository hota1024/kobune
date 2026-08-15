//! What an uninstall finds, against a real Docker.
//!
//! Storage is the part of a purge that cannot be checked against a
//! fixture. Whether a volume is really labelled the way the listing filter
//! expects, and whether Docker really lets go of it, are questions only
//! Docker answers — and both were wrong at some point in a backend whose
//! unit tests passed (`docs/DESIGN.md` §6).
//!
//! **Nothing here runs a purge for real, and nothing should.** The sweep
//! is deliberately machine-wide: it finds every volume carrying Kobune's
//! label, whatever project it belongs to and whether or not the daemon
//! remembers that project. That is what makes it able to reclaim the
//! storage of a repository somebody has already deleted — and what would
//! make `Purge { dry_run: false }`, run here, delete the development
//! databases of whoever is running the suite. A test that wants to see a
//! volume go removes the one it made itself, through the runtime.
//!
//! `#[ignore]`d like every other suite that needs a runtime:
//!
//! ```console
//! $ cargo test -p kobuned -- --ignored --test-threads=1
//! ```

use std::process::Command;

use kobune_api::{Request, Response};
use kobune_runtime::{ManagedVolume, Runtime};

#[macro_use]
mod common;

use common::Harness;

/// One service, and a project-scoped volume under it.
///
/// `pgdata` without a `@workspace` suffix is the default scope and the
/// case that matters here: it is shared between worktrees and outlives all
/// of them, so nothing on the `kobune rm` path ever removes it.
fn web_with_storage(project: &str) -> String {
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
volumes = ["pgdata:/data"]
idle_timeout = "1s"
"#
    )
}

/// The Docker backend, connected fresh.
fn runtime() -> kobune_runtime::DockerRuntime {
    kobune_runtime::docker::DockerRuntime::connect().expect("Docker answers")
}

/// This project's volumes, as the runtime reports them.
///
/// **Narrowed to the project under test.** The listing is machine-wide by
/// design, so anything wider would be asserting on whatever else the
/// machine happens to be running.
async fn volumes_of(project: &str) -> Vec<ManagedVolume> {
    let mut mine: Vec<ManagedVolume> = runtime()
        .managed_volumes()
        .await
        .expect("lists the volumes")
        .into_iter()
        .filter(|volume| volume.project == project)
        .collect();

    mine.sort();
    mine
}

/// Removes the project's containers, the way a purge does before it
/// reaches the storage.
///
/// Docker refuses to remove a volume any container still refers to,
/// running or stopped, so this is a precondition rather than a tidy-up:
/// without it the removal below would fail for a reason that has nothing
/// to do with what is being tested.
fn remove_containers(project: &str) {
    let filter = format!("label={}={}", kobune_runtime::labels::PROJECT, project);

    let ids = Command::new("docker")
        .args(["ps", "-aq", "--filter", &filter])
        .output()
        .expect("docker answers");

    let ids: Vec<String> = String::from_utf8_lossy(&ids.stdout)
        .split_whitespace()
        .map(str::to_string)
        .collect();

    if !ids.is_empty() {
        let removed = Command::new("docker")
            .arg("rm")
            .arg("-f")
            .args(&ids)
            .output()
            .expect("docker answers");

        assert!(
            removed.status.success(),
            "cannot remove the containers: {}",
            String::from_utf8_lossy(&removed.stderr)
        );
    }
}

#[tokio::test]
#[ignore = "needs a Docker daemon"]
async fn the_plan_names_the_storage_before_anything_is_asked() {
    require_docker!();

    // The whole point of listing it. A project volume is where a
    // development database ends up, and an uninstall that removed one
    // without ever naming it would be asking "remove all of this?" about
    // something the person could not see.
    let project = "mnte2evolplan";
    let harness = Harness::new(project, &web_with_storage(project));

    harness.up().await;

    let volume = format!("kobune-{project}-pgdata");

    let Response::Purge(report) = harness.request(Request::Purge { dry_run: true }).await else {
        panic!("a purge should answer with a report");
    };

    assert!(
        report.volumes.iter().any(|listed| listed.name == volume),
        "the plan left the storage out: {:?}",
        report.volumes
    );

    // A dry run that removed anything would be the worst possible bug in
    // this path, so it is pinned rather than assumed.
    assert!(
        volumes_of(project).await.iter().any(|v| v.id == volume),
        "the dry run removed the volume"
    );
}

#[tokio::test]
#[ignore = "needs a Docker daemon"]
async fn storage_no_worktree_is_left_to_claim_is_still_found_and_removed() {
    require_docker!();

    // The gap this suite exists for. `destroy_workspace` takes the
    // workspace-scoped volumes and leaves the project's — correctly, since
    // another worktree may still want it — so after the last worktree has
    // gone the storage is still there, under a name Kobune invented and
    // nothing else knows to look for.
    let project = "mnte2evolgone";
    let harness = Harness::new(project, &web_with_storage(project));

    harness.up().await;
    remove_containers(project);

    let volume = format!("kobune-{project}-pgdata");

    let found = volumes_of(project).await;
    assert!(
        found.iter().any(|listed| listed.id == volume),
        "the storage was not found once its containers had gone: {found:?}"
    );

    for listed in &found {
        runtime()
            .remove_managed_volume(listed)
            .await
            .unwrap_or_else(|err| panic!("{} should have been removed: {err}", listed.id));
    }

    assert!(
        volumes_of(project).await.is_empty(),
        "the storage survived being removed"
    );
}
