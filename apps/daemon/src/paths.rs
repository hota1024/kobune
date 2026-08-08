//! Keeping configured paths inside the tree they name.
//!
//! Two callers want the same four moves — resolve, resolve the root,
//! compare, refuse — and they want to phrase the refusal differently.
//! Sharing the moves and not the wording is what keeps them from drifting:
//! a check that grows a case on one side and not the other is a hole nobody
//! notices.

use std::path::{Path, PathBuf};

/// Why a path could not be accepted.
#[derive(Debug)]
pub enum Containment {
    /// It could not be resolved at all.
    Unresolvable(std::io::Error),
    /// It resolves somewhere outside the root, with the place it lands.
    Outside(PathBuf),
}

/// Resolves `candidate` and requires it to sit under `root`.
///
/// **Resolved, not merely joined.** `..` is the obvious way out and the
/// easy one to catch; a symlink is neither, and only the resolved path
/// shows it.
///
/// The candidate has to exist — there is nothing to resolve otherwise.
/// Whether that is an error is the caller's to decide, so it is left to
/// them rather than folded in here.
pub fn resolve_within(root: &Path, candidate: &Path) -> Result<PathBuf, Containment> {
    let resolved = candidate
        .canonicalize()
        .map_err(Containment::Unresolvable)?;

    // A root that cannot be resolved is compared as given. It is the tree
    // being worked in, so failing here means something is wrong that this
    // check is not the place to report.
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());

    if !resolved.starts_with(&root) {
        return Err(Containment::Outside(resolved));
    }

    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).expect("creates");
        (dir, root)
    }

    #[test]
    fn accepts_something_inside() {
        let (_dir, root) = dirs();
        std::fs::write(root.join("file"), "x").expect("writes");

        let resolved = resolve_within(&root, &root.join("file")).expect("is inside");
        assert!(resolved.ends_with("file"));
    }

    #[test]
    fn refuses_a_parent_escape() {
        let (dir, root) = dirs();
        std::fs::write(dir.path().join("outside"), "x").expect("writes");

        let err = resolve_within(&root, &root.join("../outside")).unwrap_err();
        assert!(matches!(err, Containment::Outside(_)), "{err:?}");
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_symlink_that_leaves() {
        // The case `..` checking cannot see: the path as written stays
        // inside, and only what it resolves to leaves.
        let (dir, root) = dirs();
        let outside = dir.path().join("outside");
        std::fs::write(&outside, "x").expect("writes");
        std::os::unix::fs::symlink(&outside, root.join("link")).expect("links");

        let err = resolve_within(&root, &root.join("link")).unwrap_err();
        assert!(matches!(err, Containment::Outside(_)), "{err:?}");
    }

    #[test]
    fn a_sibling_directory_does_not_count_as_inside() {
        // `starts_with` is by component, so `repo-evil` must not match
        // `repo` the way a plain string prefix would.
        let (dir, root) = dirs();
        let sibling = dir.path().join("repo-evil");
        std::fs::create_dir_all(&sibling).expect("creates");
        std::fs::write(sibling.join("file"), "x").expect("writes");

        let err = resolve_within(&root, &sibling.join("file")).unwrap_err();
        assert!(matches!(err, Containment::Outside(_)), "{err:?}");
    }

    #[test]
    fn something_that_is_not_there_cannot_be_resolved() {
        let (_dir, root) = dirs();

        let err = resolve_within(&root, &root.join("missing")).unwrap_err();
        assert!(matches!(err, Containment::Unresolvable(_)), "{err:?}");
    }
}
