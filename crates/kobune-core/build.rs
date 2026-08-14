//! Records which commit the binary was built from.
//!
//! Every nightly build reports version 0.1.0, so the version alone cannot
//! answer "is there something newer than what I am running". The commit can.

use std::process::Command;

fn main() {
    // CI passes the commit it is building; locally it comes from git. Both
    // are declared below so a change to either rebuilds this.
    println!("cargo:rerun-if-env-changed=KOBUNE_BUILD_COMMIT");
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");

    let commit = std::env::var("KOBUNE_BUILD_COMMIT")
        .or_else(|_| std::env::var("GITHUB_SHA"))
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(git_commit)
        // A source tarball with no git and no CI. Better than refusing to
        // build over something only the update check reads.
        .unwrap_or_else(|| "unknown".to_string());

    let short: String = commit.chars().take(SHORT_LEN).collect();

    println!("cargo:rustc-env=KOBUNE_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=KOBUNE_BUILD_COMMIT_SHORT={short}");

    // Which release archive this build corresponds to. `std::env::consts`
    // gives the OS and architecture separately, and stitching them back
    // into a triple means duplicating the mapping cargo already did.
    let target = std::env::var("TARGET").expect("cargo always sets TARGET");
    println!("cargo:rustc-env=KOBUNE_BUILD_TARGET={target}");
}

/// How much of the hash to show a person. Matches `git log --oneline`.
const SHORT_LEN: usize = 7;

fn git_commit() -> Option<String> {
    // No rerun-if-changed on .git: a build script cannot watch a directory
    // whose layout differs between a clone, a worktree and a submodule, and
    // being one commit stale locally costs nothing. CI passes the value.
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let commit = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!commit.is_empty()).then_some(commit)
}
