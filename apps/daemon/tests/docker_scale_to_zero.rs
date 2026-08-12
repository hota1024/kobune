//! Starting and stopping services, against a real Docker.
//!
//! Mostly scale-to-zero, which is where the bugs were; also the one thing
//! about `up` that cannot be seen from a unit test, which is whether two
//! services actually come up at the same time.
//!
//! **Why a suite this expensive is worth it.** `docker.rs` is most of two
//! thousand lines with a dozen tests, all of them parsing fixtures written
//! by hand — the arrangement `docs/DESIGN.md` §6 blames for M0's Apple
//! Container backend being wholly broken while its tests passed. The
//! Docker backend still has that exposure, and the two bugs this file
//! starts with were both invisible to a unit test and both obvious within
//! a minute of running it.
//!
//! The project, the daemon and the clearing up are in [`common`], shared
//! with the other suites that need a real runtime.
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

use std::time::Duration;

#[macro_use]
mod common;

use common::{Harness, START_WAIT};

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
