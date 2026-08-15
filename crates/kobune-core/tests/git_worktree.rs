//! Exercises worktree handling against a real git repository.
//!
//! Worktrees are the core of Kobune, so this verifies against git's actual
//! output rather than testing the parser in isolation.

use std::path::Path;
use std::process::Command;

use kobune_core::git::Repository;

fn run(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("cannot run git: {e}"));

    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Creates an empty repository with a single commit.
fn init_repo(dir: &Path) {
    run(dir, &["init", "--initial-branch=main"]);
    run(dir, &["config", "user.email", "test@example.com"]);
    run(dir, &["config", "user.name", "Kobune Test"]);
    run(dir, &["config", "commit.gpgsign", "false"]);

    std::fs::write(dir.join("README.md"), "hello").expect("writes");
    run(dir, &["add", "."]);
    run(dir, &["commit", "-m", "initial"]);
}

/// A tempdir may contain symlinks (/var → /private/var), so normalise it
/// to compare against the real paths git reports.
fn canonical(path: &Path) -> std::path::PathBuf {
    path.canonicalize().expect("the path exists")
}

#[test]
fn discovers_main_worktree() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = canonical(tmp.path());
    init_repo(&root);

    let repo = Repository::discover(&root).expect("recognised as a repository");
    assert_eq!(repo.root, root);
    assert_eq!(repo.main_root, root);
    assert!(repo.is_main());
    assert_eq!(
        repo.current_branch().expect("succeeds").as_deref(),
        Some("main")
    );
}

#[test]
fn reports_not_a_repository_outside_git() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // Run directly in the tempdir so no real repository is found above it.
    let result = Repository::discover(tmp.path());

    // Fails unless an ancestor of the tempdir happens to be a repository.
    if let Ok(repo) = result {
        panic!("expected no repository, but found {}", repo.root.display());
    }
}

#[test]
fn adds_worktree_with_new_branch() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = canonical(tmp.path());
    init_repo(&root);

    let repo = Repository::discover(&root).expect("recognised");
    let wt_path = root.join("wt").join("feat-1");

    repo.add_worktree(&wt_path, "feature/one", None)
        .expect("the worktree is created");

    assert!(
        wt_path.join("README.md").is_file(),
        "the tree is checked out"
    );

    let worktrees = repo.worktrees().expect("enumerates");
    assert_eq!(worktrees.len(), 2);
    assert_eq!(
        worktrees[0].path, root,
        "the first entry is the main worktree"
    );

    let added = worktrees
        .iter()
        .find(|wt| wt.path == canonical(&wt_path))
        .expect("the new worktree is listed");
    assert_eq!(added.branch.as_deref(), Some("feature/one"));
    assert!(!added.detached);
}

#[test]
fn adds_worktree_reusing_existing_branch() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = canonical(tmp.path());
    init_repo(&root);
    run(&root, &["branch", "existing"]);

    let repo = Repository::discover(&root).expect("recognised");
    assert!(repo.branch_exists("existing"));
    assert!(!repo.branch_exists("nope"));

    let wt_path = root.join("wt").join("existing");
    repo.add_worktree(&wt_path, "existing", None)
        .expect("an existing branch can be checked out");

    let worktrees = repo.worktrees().expect("enumerates");
    let added = worktrees
        .iter()
        .find(|wt| wt.path == canonical(&wt_path))
        .expect("found");
    assert_eq!(added.branch.as_deref(), Some("existing"));
}

#[test]
fn discovers_main_root_from_inside_worktree() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = canonical(tmp.path());
    init_repo(&root);

    let repo = Repository::discover(&root).expect("recognised");
    let wt_path = root.join("wt").join("feat-1");
    repo.add_worktree(&wt_path, "feature/one", None)
        .expect("created");

    // The main worktree must be reachable from inside a worktree too:
    // the CLI is invoked from either directory.
    let inner = Repository::discover(&wt_path).expect("recognised");
    assert_eq!(inner.root, canonical(&wt_path));
    assert_eq!(inner.main_root, root);
    assert!(!inner.is_main());
    assert_eq!(
        inner.current_branch().expect("succeeds").as_deref(),
        Some("feature/one")
    );
}

#[test]
fn removes_worktree_but_keeps_branch() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = canonical(tmp.path());
    init_repo(&root);

    let repo = Repository::discover(&root).expect("recognised");
    let wt_path = root.join("wt").join("feat-1");
    repo.add_worktree(&wt_path, "feature/one", None)
        .expect("created");

    repo.remove_worktree(&wt_path, false).expect("removes");

    assert!(!wt_path.exists(), "the directory is gone");
    assert_eq!(repo.worktrees().expect("enumerates").len(), 1);
    assert!(
        repo.branch_exists("feature/one"),
        "removing a worktree keeps the branch"
    );
}

#[test]
fn default_base_prefers_main() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = canonical(tmp.path());
    init_repo(&root);

    let repo = Repository::discover(&root).expect("recognised");
    assert_eq!(repo.default_base().expect("succeeds"), "main");
}

#[test]
fn reports_no_remote_when_absent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = canonical(tmp.path());
    init_repo(&root);

    let repo = Repository::discover(&root).expect("recognised");
    assert_eq!(repo.remote_url().expect("succeeds"), None);
}

#[test]
fn detects_detached_head() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = canonical(tmp.path());
    init_repo(&root);

    let repo = Repository::discover(&root).expect("recognised");
    let wt_path = root.join("wt").join("detached");
    run(
        &root,
        &["worktree", "add", "--detach", &wt_path.to_string_lossy()],
    );

    let worktrees = repo.worktrees().expect("enumerates");
    let detached = worktrees
        .iter()
        .find(|wt| wt.path == canonical(&wt_path))
        .expect("found");

    assert!(detached.detached);
    assert_eq!(detached.branch, None);

    let inner = Repository::discover(&wt_path).expect("recognised");
    assert_eq!(
        inner.current_branch().expect("not an error"),
        None,
        "a detached HEAD has no branch name"
    );
}
