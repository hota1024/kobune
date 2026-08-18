//! Discovering and manipulating git worktrees.
//!
//! Shells out to `git` rather than using `gix`, so that worktree handling
//! follows git's own behaviour exactly — config, hooks, submodules and all.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{Error, Result};

/// The git repository containing `path`.
#[derive(Debug, Clone)]
pub struct Repository {
    /// The root of the worktree `path` belongs to.
    pub root: PathBuf,
    /// The main worktree's root. The anchor for listing worktrees and
    /// for branch operations.
    pub main_root: PathBuf,
}

impl Repository {
    /// Searches upwards from `path` for a git repository.
    pub fn discover(path: &Path) -> Result<Self> {
        let root = git(path, &["rev-parse", "--show-toplevel"])
            .map_err(|_| Error::NotAGitRepository(path.to_path_buf()))?;
        let root = PathBuf::from(root);

        // The first entry of `git worktree list` is always the main
        // worktree. The parent of `--git-common-dir` is not reliable for
        // bare repositories, so avoid it.
        let worktrees = list_worktrees_in(&root)?;
        let main_root = worktrees
            .first()
            .map(|wt| wt.path.clone())
            .unwrap_or_else(|| root.clone());

        Ok(Self { root, main_root })
    }

    /// Whether this is the main worktree.
    pub fn is_main(&self) -> bool {
        self.root == self.main_root
    }

    pub fn worktrees(&self) -> Result<Vec<Worktree>> {
        list_worktrees_in(&self.main_root)
    }

    /// The currently checked-out branch, or `None` on a detached HEAD.
    pub fn current_branch(&self) -> Result<Option<String>> {
        let out = git(&self.root, &["symbolic-ref", "--quiet", "--short", "HEAD"]);
        match out {
            Ok(name) if !name.is_empty() => Ok(Some(name)),
            // symbolic-ref exits non-zero on a detached HEAD.
            _ => Ok(None),
        }
    }

    pub fn remote_url(&self) -> Result<Option<String>> {
        match git(&self.main_root, &["remote", "get-url", "origin"]) {
            Ok(url) if !url.is_empty() => Ok(Some(url)),
            _ => Ok(None),
        }
    }

    pub fn branch_exists(&self, branch: &str) -> bool {
        git(
            &self.main_root,
            &[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
        )
        .is_ok()
    }

    /// The base for new worktrees: `origin/HEAD`, then `main`, then `master`.
    pub fn default_base(&self) -> Result<String> {
        if let Ok(head) = git(
            &self.main_root,
            &[
                "symbolic-ref",
                "--quiet",
                "--short",
                "refs/remotes/origin/HEAD",
            ],
        ) && !head.is_empty()
        {
            return Ok(head);
        }

        for candidate in ["main", "master"] {
            if self.branch_exists(candidate) {
                return Ok(candidate.to_string());
            }
        }

        // Fall back to the current HEAD when nothing else matches.
        Ok("HEAD".to_string())
    }

    /// Adds a worktree.
    ///
    /// Checks out `branch` if it exists, otherwise creates it from `base`.
    pub fn add_worktree(&self, path: &Path, branch: &str, base: Option<&str>) -> Result<()> {
        let path_str = path.to_string_lossy().to_string();

        if self.branch_exists(branch) {
            git(&self.main_root, &["worktree", "add", &path_str, branch])?;
        } else {
            let base = match base {
                Some(b) => b.to_string(),
                None => self.default_base()?,
            };
            git(
                &self.main_root,
                &["worktree", "add", "-b", branch, &path_str, &base],
            )?;
        }

        Ok(())
    }

    /// Removes a worktree. The branch itself is kept.
    pub fn remove_worktree(&self, path: &Path, force: bool) -> Result<()> {
        let path_str = path.to_string_lossy().to_string();
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        args.push(&path_str);

        git(&self.main_root, &args)?;
        Ok(())
    }
}

/// One entry of `git worktree list --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    pub head: Option<String>,
    /// The checked-out branch, or `None` on a detached HEAD.
    pub branch: Option<String>,
    pub detached: bool,
    pub bare: bool,
    pub locked: bool,
}

fn list_worktrees_in(dir: &Path) -> Result<Vec<Worktree>> {
    let output = git(dir, &["worktree", "list", "--porcelain"])?;
    Ok(parse_worktree_list(&output))
}

/// Parses the output of `git worktree list --porcelain`.
///
/// Entries are separated by blank lines and start with `worktree <path>`.
fn parse_worktree_list(output: &str) -> Vec<Worktree> {
    let mut result = Vec::new();
    let mut current: Option<Worktree> = None;

    for line in output.lines() {
        let line = line.trim_end();

        if line.is_empty() {
            if let Some(wt) = current.take() {
                result.push(wt);
            }
            continue;
        }

        let (key, value) = match line.split_once(' ') {
            Some((k, v)) => (k, Some(v)),
            None => (line, None),
        };

        match key {
            "worktree" => {
                if let Some(wt) = current.take() {
                    result.push(wt);
                }
                current = Some(Worktree {
                    path: PathBuf::from(value.unwrap_or_default()),
                    head: None,
                    branch: None,
                    detached: false,
                    bare: false,
                    locked: false,
                });
            }
            "HEAD" => {
                if let Some(wt) = current.as_mut() {
                    wt.head = value.map(str::to_string);
                }
            }
            "branch" => {
                if let Some(wt) = current.as_mut() {
                    // `refs/heads/feature/x` → `feature/x`
                    wt.branch =
                        value.map(|v| v.strip_prefix("refs/heads/").unwrap_or(v).to_string());
                }
            }
            "detached" => {
                if let Some(wt) = current.as_mut() {
                    wt.detached = true;
                }
            }
            "bare" => {
                if let Some(wt) = current.as_mut() {
                    wt.bare = true;
                }
            }
            "locked" => {
                if let Some(wt) = current.as_mut() {
                    wt.locked = true;
                }
            }
            _ => {}
        }
    }

    if let Some(wt) = current.take() {
        result.push(wt);
    }

    result
}

/// Whether git tracks `relative` within `worktree`.
///
/// **A file Kobune generates must not be one git is watching.** Writing it
/// would leave the worktree permanently dirty, and committing it would put
/// one branch's URLs into every other checkout.
pub fn is_tracked(worktree: &Path, relative: &str) -> bool {
    git(worktree, &["ls-files", "--error-unmatch", "--", relative]).is_ok()
}

/// Whether git's ignore rules already cover `relative`.
///
/// **Asked of git rather than read off `.gitignore`.** The name may be
/// covered by a pattern, by `.git/info/exclude`, or by the user's global
/// ignore file, and an entry appended underneath one of those is noise in
/// somebody's diff for nothing.
///
/// `--no-index` asks about the rules alone. Without it a path that is
/// already tracked reads as not ignored, which is true of what git will do
/// and not an answer to whether a rule for it exists — and adding a second
/// rule would not untrack it either.
///
/// Not a [`Result`]: `check-ignore` exits 1 for "no rule covers this",
/// which is an answer rather than a failure, and so is being outside a
/// repository. Both come back as false, which is what a caller deciding
/// whether to write a rule wants from either.
pub fn is_ignored(worktree: &Path, relative: &str) -> bool {
    Command::new("git")
        .current_dir(worktree)
        .args(["check-ignore", "--quiet", "--no-index", "--", relative])
        .output()
        .is_ok_and(|output| output.status.success())
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .map_err(Error::GitSpawn)?;

    if !output.status.success() {
        return Err(Error::GitFailed {
            args: args.join(" "),
            code: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_output() {
        let output = "\
worktree /repo/main
HEAD abc123
branch refs/heads/main

worktree /repo/wt/feat
HEAD def456
branch refs/heads/feature/user-auth

worktree /repo/wt/detached
HEAD 789abc
detached
";
        let list = parse_worktree_list(output);
        assert_eq!(list.len(), 3);

        assert_eq!(list[0].path, PathBuf::from("/repo/main"));
        assert_eq!(list[0].branch.as_deref(), Some("main"));
        assert!(!list[0].detached);

        assert_eq!(list[1].branch.as_deref(), Some("feature/user-auth"));

        assert!(list[2].detached);
        assert_eq!(list[2].branch, None);
    }

    #[test]
    fn parses_bare_and_locked() {
        let output = "\
worktree /repo/bare
bare

worktree /repo/wt
HEAD abc
branch refs/heads/x
locked
";
        let list = parse_worktree_list(output);
        assert_eq!(list.len(), 2);
        assert!(list[0].bare);
        assert!(list[1].locked);
    }

    #[test]
    fn tolerates_missing_trailing_blank_line() {
        let output = "worktree /repo/main\nHEAD abc\nbranch refs/heads/main";
        let list = parse_worktree_list(output);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].branch.as_deref(), Some("main"));
    }

    #[test]
    fn parses_empty_output() {
        assert!(parse_worktree_list("").is_empty());
    }
}
