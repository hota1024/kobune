//! 実際に git リポジトリを作って worktree 操作を確認する。
//!
//! worktree の扱いは Minato の中核なので、パーサ単体ではなく
//! 本物の git の出力に対して検証する。

use std::path::Path;
use std::process::Command;

use minato_core::git::Repository;

fn run(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git を起動できません: {e}"));

    assert!(
        output.status.success(),
        "git {args:?} が失敗しました: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// コミットが 1 つある空のリポジトリを作る。
fn init_repo(dir: &Path) {
    run(dir, &["init", "--initial-branch=main"]);
    run(dir, &["config", "user.email", "test@example.com"]);
    run(dir, &["config", "user.name", "Minato Test"]);
    run(dir, &["config", "commit.gpgsign", "false"]);

    std::fs::write(dir.join("README.md"), "hello").expect("書ける");
    run(dir, &["add", "."]);
    run(dir, &["commit", "-m", "initial"]);
}

/// tempdir はシンボリックリンク (/var → /private/var) を含みうるので、
/// git が返す実パスと比較できるように正規化する。
fn canonical(path: &Path) -> std::path::PathBuf {
    path.canonicalize().expect("存在するパス")
}

#[test]
fn discovers_main_worktree() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = canonical(tmp.path());
    init_repo(&root);

    let repo = Repository::discover(&root).expect("リポジトリとして認識される");
    assert_eq!(repo.root, root);
    assert_eq!(repo.main_root, root);
    assert!(repo.is_main());
    assert_eq!(
        repo.current_branch().expect("取得できる").as_deref(),
        Some("main")
    );
}

#[test]
fn reports_not_a_repository_outside_git() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // 親を辿って本物のリポジトリに当たらないよう、tempdir 直下で試す。
    let result = Repository::discover(tmp.path());

    // tempdir の祖先がリポジトリでない限りエラーになる。
    if let Ok(repo) = result {
        panic!(
            "git 管理下でないはずが {} を検出しました",
            repo.root.display()
        );
    }
}

#[test]
fn adds_worktree_with_new_branch() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = canonical(tmp.path());
    init_repo(&root);

    let repo = Repository::discover(&root).expect("認識される");
    let wt_path = root.join("wt").join("feat-1");

    repo.add_worktree(&wt_path, "feature/one", None)
        .expect("worktree を作れる");

    assert!(
        wt_path.join("README.md").is_file(),
        "作業ツリーが展開される"
    );

    let worktrees = repo.worktrees().expect("列挙できる");
    assert_eq!(worktrees.len(), 2);
    assert_eq!(worktrees[0].path, root, "先頭は main worktree");

    let added = worktrees
        .iter()
        .find(|wt| wt.path == canonical(&wt_path))
        .expect("追加した worktree が見つかる");
    assert_eq!(added.branch.as_deref(), Some("feature/one"));
    assert!(!added.detached);
}

#[test]
fn adds_worktree_reusing_existing_branch() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = canonical(tmp.path());
    init_repo(&root);
    run(&root, &["branch", "existing"]);

    let repo = Repository::discover(&root).expect("認識される");
    assert!(repo.branch_exists("existing"));
    assert!(!repo.branch_exists("nope"));

    let wt_path = root.join("wt").join("existing");
    repo.add_worktree(&wt_path, "existing", None)
        .expect("既存ブランチをチェックアウトできる");

    let worktrees = repo.worktrees().expect("列挙できる");
    let added = worktrees
        .iter()
        .find(|wt| wt.path == canonical(&wt_path))
        .expect("見つかる");
    assert_eq!(added.branch.as_deref(), Some("existing"));
}

#[test]
fn discovers_main_root_from_inside_worktree() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = canonical(tmp.path());
    init_repo(&root);

    let repo = Repository::discover(&root).expect("認識される");
    let wt_path = root.join("wt").join("feat-1");
    repo.add_worktree(&wt_path, "feature/one", None)
        .expect("作れる");

    // worktree の中から見ても main worktree を指せる必要がある。
    // CLI はどちらのディレクトリからも呼ばれるため。
    let inner = Repository::discover(&wt_path).expect("認識される");
    assert_eq!(inner.root, canonical(&wt_path));
    assert_eq!(inner.main_root, root);
    assert!(!inner.is_main());
    assert_eq!(
        inner.current_branch().expect("取得できる").as_deref(),
        Some("feature/one")
    );
}

#[test]
fn removes_worktree_but_keeps_branch() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = canonical(tmp.path());
    init_repo(&root);

    let repo = Repository::discover(&root).expect("認識される");
    let wt_path = root.join("wt").join("feat-1");
    repo.add_worktree(&wt_path, "feature/one", None)
        .expect("作れる");

    repo.remove_worktree(&wt_path, false).expect("削除できる");

    assert!(!wt_path.exists(), "ディレクトリが消える");
    assert_eq!(repo.worktrees().expect("列挙できる").len(), 1);
    assert!(
        repo.branch_exists("feature/one"),
        "worktree を消してもブランチは残す"
    );
}

#[test]
fn default_base_prefers_main() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = canonical(tmp.path());
    init_repo(&root);

    let repo = Repository::discover(&root).expect("認識される");
    assert_eq!(repo.default_base().expect("取得できる"), "main");
}

#[test]
fn reports_no_remote_when_absent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = canonical(tmp.path());
    init_repo(&root);

    let repo = Repository::discover(&root).expect("認識される");
    assert_eq!(repo.remote_url().expect("取得できる"), None);
}

#[test]
fn detects_detached_head() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = canonical(tmp.path());
    init_repo(&root);

    let repo = Repository::discover(&root).expect("認識される");
    let wt_path = root.join("wt").join("detached");
    run(
        &root,
        &["worktree", "add", "--detach", &wt_path.to_string_lossy()],
    );

    let worktrees = repo.worktrees().expect("列挙できる");
    let detached = worktrees
        .iter()
        .find(|wt| wt.path == canonical(&wt_path))
        .expect("見つかる");

    assert!(detached.detached);
    assert_eq!(detached.branch, None);

    let inner = Repository::discover(&wt_path).expect("認識される");
    assert_eq!(
        inner.current_branch().expect("エラーにはしない"),
        None,
        "detached HEAD ではブランチ名を返さない"
    );
}
