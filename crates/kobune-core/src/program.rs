//! Finding the programs Minato shells out to.
//!
//! A daemon launchd starts inherits `PATH=/usr/bin:/bin:/usr/sbin:/sbin` and
//! nothing else. Those four directories are on macOS's sealed system volume,
//! so nothing can be installed into them — which means a bare command name
//! resolves to nothing at all from inside the daemon, however the binary got
//! onto the machine. Homebrew, MacPorts, `go install`, mise and a
//! hand-placed download all land somewhere launchd has never heard of, and
//! the daemon reports a CLI the user is looking at as "not installed".
//!
//! Looking the name up here and spawning the absolute path is what closes
//! that gap. **Not a `PATH` in the plist**: that file is installed with
//! `sudo`, so every addition to the list would put a privileged step in
//! front of everyone who already has Minato working.
//!
//! Docker needs none of this. `minato-runtime`'s Docker backend talks to the
//! Engine API over a socket and spawns nothing, which is why it is the one
//! runtime this was never noticed on.

use std::env;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Where a package manager puts binaries when it is not on `PATH`.
///
/// Guesses, and deliberately short ones: a name that is genuinely absent has
/// to reach the "not installed" answer promptly, and every entry here is a
/// `stat` on the way to it.
const PREFIXES: &[&str] = &[
    // Homebrew on Apple Silicon.
    "/opt/homebrew/bin",
    // Homebrew on Intel, Apple Container's installer, and where a download
    // from a project's releases page is usually put.
    "/usr/local/bin",
    // MacPorts.
    "/opt/local/bin",
    // nix-darwin's system profile.
    "/run/current-system/sw/bin",
];

/// The same, relative to the user's home.
const HOME_PREFIXES: &[&str] = &[
    ".local/bin",
    "bin",
    // `go install`, which is how cloudflared is built from source.
    "go/bin",
    // Nix without nix-darwin.
    ".nix-profile/bin",
    // Version managers, which hand out shims rather than the binary.
    ".local/share/mise/shims",
    ".asdf/shims",
];

/// The command to spawn for `program`.
///
/// The absolute path when there is one, and `program` unchanged when there
/// is not: what follows a failed lookup is a spawn that fails with "no such
/// file", and the bare name is what that message should carry.
pub fn resolve(program: &str) -> String {
    find(program)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| program.to_string())
}

/// The same, letting an override stand in for `program`.
///
/// For the `MINATO_*` variables that point at a binary Minato would not find
/// on its own, and that tests point at a stub. An exported-but-empty
/// variable is how a shell says "unset", so it does not count as one.
pub fn resolve_with(override_value: Option<&str>, program: &str) -> String {
    resolve(
        override_value
            .filter(|value| !value.is_empty())
            .unwrap_or(program),
    )
}

/// Looks `program` up, without running it.
///
/// Readiness is reported before anything is spawned, so the lookup has to be
/// answerable on its own.
pub fn find(program: &str) -> Option<PathBuf> {
    find_in(
        program,
        &search_dirs(
            env::var_os("PATH").as_deref(),
            env::var_os("HOME").as_deref(),
        ),
    )
}

/// The lookup itself, against the directories it is given.
///
/// Split out from [`find`] so it can be tested without setting a process
/// variable. Setting one would race every other test in the binary, and
/// `PATH` is the variable the rest of them are least able to survive losing.
fn find_in(program: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    // A name with a separator in it is a path already. That is how a test
    // points a runtime at a stub, and how an override reaches the spawn
    // unchanged even when it names something that is not there.
    if program.contains('/') {
        let path = PathBuf::from(program);
        return runnable(&path).then_some(path);
    }

    dirs.iter().find_map(|dir| {
        let candidate = dir.join(program);
        runnable(&candidate).then_some(candidate)
    })
}

/// The directories to look in, in order.
///
/// `PATH` first: where the user put the thing outranks a guess about where
/// it might be, and a machine with two copies installed has already said
/// which one it means.
fn search_dirs(path: Option<&OsStr>, home: Option<&OsStr>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = path
        .map(|path| env::split_paths(path).collect())
        .unwrap_or_default();

    dirs.extend(PREFIXES.iter().map(PathBuf::from));

    if let Some(home) = home.map(Path::new) {
        dirs.extend(HOME_PREFIXES.iter().map(|dir| home.join(dir)));
    }

    dirs
}

/// Whether this is something that can be executed.
///
/// The permission bits and not merely "a file exists there": the prefixes
/// above are shared directories, and a stray file that happens to carry a
/// program's name would otherwise be spawned instead of it.
fn runnable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        // `metadata` follows symlinks, which is what Homebrew's bin holds.
        path.metadata()
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }

    #[cfg(not(unix))]
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file that can be run, and one that cannot.
    fn write(dir: &Path, name: &str, executable: bool) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\n").expect("writes");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if executable { 0o755 } else { 0o644 };
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
        }

        path
    }

    #[test]
    fn finds_a_program_in_one_of_the_directories() {
        let dir = tempfile::tempdir().expect("temp dir");
        let expected = write(dir.path(), "cloudflared", true);

        assert_eq!(
            find_in("cloudflared", &[dir.path().to_path_buf()]),
            Some(expected)
        );
    }

    #[test]
    fn a_name_that_is_nowhere_is_not_found() {
        let dir = tempfile::tempdir().expect("temp dir");

        assert_eq!(find_in("cloudflared", &[dir.path().to_path_buf()]), None);
    }

    #[test]
    #[cfg(unix)]
    fn a_file_that_cannot_be_run_is_not_the_program() {
        // The prefixes are shared directories. Answering with something that
        // cannot be executed would turn "not installed" into a spawn failure
        // nobody can act on.
        let dir = tempfile::tempdir().expect("temp dir");
        write(dir.path(), "container", false);

        assert_eq!(find_in("container", &[dir.path().to_path_buf()]), None);
    }

    #[test]
    fn the_first_directory_that_has_it_wins() {
        let first = tempfile::tempdir().expect("temp dir");
        let second = tempfile::tempdir().expect("temp dir");
        let expected = write(first.path(), "container", true);
        write(second.path(), "container", true);

        assert_eq!(
            find_in(
                "container",
                &[first.path().to_path_buf(), second.path().to_path_buf()]
            ),
            Some(expected)
        );
    }

    #[test]
    fn a_path_is_taken_as_given() {
        let dir = tempfile::tempdir().expect("temp dir");
        let stub = write(dir.path(), "stub", true);
        let name = stub.to_string_lossy().into_owned();

        // Nowhere to search, and it is still found: the caller named a file.
        assert_eq!(find_in(&name, &[]), Some(stub));
        assert_eq!(find_in("/nowhere/cloudflared", &[]), None);
    }

    #[test]
    fn path_is_searched_before_the_guesses() {
        let dirs = search_dirs(
            Some(OsStr::new("/one:/two")),
            Some(OsStr::new("/home/someone")),
        );

        assert_eq!(dirs[0], Path::new("/one"));
        assert_eq!(dirs[1], Path::new("/two"));
        assert_eq!(dirs[2], Path::new(PREFIXES[0]));
    }

    #[test]
    fn the_home_prefixes_are_expanded() {
        let dirs = search_dirs(None, Some(OsStr::new("/home/someone")));

        assert!(
            dirs.contains(&PathBuf::from("/home/someone/.local/bin")),
            "got: {dirs:?}"
        );
    }

    #[test]
    fn no_environment_at_all_still_leaves_the_guesses() {
        // A launchd job is handed very little, and a lookup that gave up
        // here would be the bug this module exists for.
        let dirs = search_dirs(None, None);

        assert_eq!(dirs, PREFIXES.iter().map(PathBuf::from).collect::<Vec<_>>());
    }

    #[test]
    fn an_override_stands_in_for_the_program() {
        assert_eq!(
            resolve_with(Some("/opt/custom/cloudflared"), "cloudflared"),
            "/opt/custom/cloudflared"
        );

        // An exported-but-empty variable is how a shell says "unset".
        assert!(resolve_with(Some(""), "cloudflared").ends_with("cloudflared"));
    }

    #[test]
    fn a_name_that_is_nowhere_resolves_to_itself() {
        // The spawn that follows fails with "no such file", and the name is
        // what makes that message worth reading.
        assert_eq!(
            resolve("minato-definitely-not-a-real-program"),
            "minato-definitely-not-a-real-program"
        );
    }
}
