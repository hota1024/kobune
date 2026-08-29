//! Finding the workspace a loosely-typed name means.
//!
//! **Not `#[ignore]`d, unlike its neighbours.** `Find` is the one request
//! that answers without asking the runtime anything — a directory and a
//! label are known from git and the state file alone — so this suite
//! needs Docker no more than `git worktree list` does, and the whole
//! point of the request is that it is cheap.
//!
//! What a unit test cannot reach is here: real worktrees, made by git,
//! registered by the daemon on the way past.

use kobune_api::{ErrorCode, Request, Response, Target};

#[macro_use]
mod common;

use common::{Harness, git};

fn config(project: &str) -> String {
    format!(
        r#"
[project]
name = "{project}"

[services.web]
image = "busybox:latest"
port = 8000
"#
    )
}

/// A project whose main worktree has `feature/user-auth` and `fix/auth`
/// beside it — enough for one name to fit two workspaces.
fn three_worktrees() -> Harness {
    let harness = Harness::new("kobune-find", &config("kobune-find"));

    for branch in ["feature/user-auth", "fix/auth"] {
        let path = harness.root.join(".wt").join(branch.replace('/', "-"));
        git(
            &harness.root,
            &[
                "worktree",
                "add",
                "-b",
                branch,
                &path.to_string_lossy(),
                "main",
            ],
        );
    }

    harness
}

async fn find(harness: &Harness, query: Option<&str>) -> Result<Vec<String>, ErrorCode> {
    let request = Request::Find {
        target: harness.target(),
        query: query.map(str::to_string),
        candidates: false,
    };

    let (_keys, from_client) = tokio::sync::mpsc::unbounded_channel();
    let outcome = harness
        .supervisor
        .handle(request, &kobune_runtime::EventSink::discard(), from_client)
        .await;

    match outcome {
        Ok(Response::Locations { workspaces }) => Ok(workspaces
            .into_iter()
            .map(|workspace| workspace.workspace)
            .collect()),
        Ok(other) => panic!("expected locations, got {other:?}"),
        Err(err) => Err(err.code),
    }
}

/// `kobune cd` with nothing after it.
#[tokio::test]
async fn no_name_at_all_means_the_main_worktree() {
    let harness = three_worktrees();

    assert_eq!(find(&harness, None).await, Ok(vec!["main".to_string()]));
}

/// The worktrees were made with `git worktree add` rather than with
/// `kobune new`, so nothing had registered them before this asked.
#[tokio::test]
async fn a_worktree_kobune_has_never_seen_can_still_be_moved_to() {
    let harness = three_worktrees();

    assert_eq!(
        find(&harness, Some("feature/user-auth")).await,
        Ok(vec!["feature-user-auth".to_string()])
    );
}

#[tokio::test]
async fn characters_in_order_are_enough_of_a_name() {
    let harness = three_worktrees();

    assert_eq!(
        find(&harness, Some("fuauth")).await,
        Ok(vec!["feature-user-auth".to_string()])
    );
}

/// `auth` sits inside both labels the same way, and picking either would
/// be a guess about which directory somebody meant.
#[tokio::test]
async fn a_name_that_fits_two_is_refused_rather_than_guessed_at() {
    let harness = three_worktrees();

    assert_eq!(
        find(&harness, Some("auth")).await,
        Err(ErrorCode::Ambiguous)
    );
}

#[tokio::test]
async fn a_name_that_fits_nothing_is_not_found() {
    let harness = three_worktrees();

    assert_eq!(find(&harness, Some("nope")).await, Err(ErrorCode::NotFound));
}

/// `git worktree list` keeps naming a worktree whose directory was
/// deleted until somebody prunes it, and a `cd` to one of those would be
/// a shell sent somewhere that is not there.
#[tokio::test]
async fn a_worktree_whose_directory_has_gone_is_not_somewhere_to_move_to() {
    let harness = three_worktrees();

    std::fs::remove_dir_all(harness.root.join(".wt").join("fix-auth")).expect("removes it");

    assert_eq!(
        find(&harness, Some("fix-auth")).await,
        Err(ErrorCode::NotFound)
    );

    // The others are unaffected, so this is not "the listing fell over".
    assert_eq!(
        find(&harness, Some("fuauth")).await,
        Ok(vec!["feature-user-auth".to_string()])
    );
}

/// What a shell completion asks: everything, and no error for a name
/// that is still being typed.
#[tokio::test]
async fn candidates_are_listed_for_a_completion() {
    let harness = three_worktrees();

    let request = Request::Find {
        target: Target::new(harness.root.clone()),
        query: None,
        candidates: true,
    };

    let Response::Locations { workspaces } = harness.request(request).await else {
        panic!("expected locations");
    };

    let labels: Vec<&str> = workspaces
        .iter()
        .map(|workspace| workspace.workspace.as_str())
        .collect();

    assert_eq!(labels, ["feature-user-auth", "fix-auth", "main"]);

    // The path is the answer `cd` is after, and the main worktree keeps a
    // name here even though a URL leaves it out.
    let main = workspaces
        .iter()
        .find(|workspace| workspace.is_main)
        .expect("the main worktree is one of them");

    assert_eq!(main.path, harness.root);
    assert_eq!(main.workspace, "main");
}

#[tokio::test]
async fn half_a_name_narrows_the_candidates_without_failing() {
    let harness = three_worktrees();

    let request = Request::Find {
        target: harness.target(),
        query: Some("auth".to_string()),
        candidates: true,
    };

    let Response::Locations { workspaces } = harness.request(request).await else {
        panic!("expected locations");
    };

    let labels: Vec<&str> = workspaces
        .iter()
        .map(|workspace| workspace.workspace.as_str())
        .collect();

    assert_eq!(labels, ["feature-user-auth", "fix-auth"]);
}
