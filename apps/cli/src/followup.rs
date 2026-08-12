//! What is worth running once the binaries have changed underneath.
//!
//! An update swaps `minato` and `minatod` and leaves the machine halfway:
//! the daemon on the socket is the build that has just been replaced, the
//! Skill sitting in a repository is a copy of the one another build
//! carried, and a plist written by an older build may no longer be the
//! shape this one expects. None of that is broken enough to fail on, and
//! each of it is settled by one command — so the commands are what this
//! works out.
//!
//! **Every check is local, and every one of them is a fact rather than a
//! guess.** A step appears because something on this machine says so: a
//! daemon answered, a file differs, a revision is behind. When there is
//! nothing to compare against — a plist from before revisions existed, a
//! build with no commit of its own — nothing is claimed and nothing is
//! printed. A list that is right most of the time is one people stop
//! reading.
//!
//! The wording of the steps is here; how they are *drawn* is
//! [`crate::ui`]'s, as everywhere else.

use std::path::{Path, PathBuf};

use minato_client::Client;
use serde::{Deserialize, Serialize};

/// One command worth running, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Step {
    /// The command, as it should be typed.
    pub command: String,
    /// What makes it worth running, as a statement about this machine.
    /// It is what `--json` carries, and what the notice puts in front of
    /// the command.
    pub reason: String,
}

impl Step {
    fn new(reason: &str, command: &str) -> Self {
        Self {
            command: command.to_string(),
            reason: reason.to_string(),
        }
    }
}

/// What is answering the daemon socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Daemon {
    /// Nothing is.
    Stopped,
    /// A daemon from this build.
    Current,
    /// A daemon from some other build.
    Previous,
}

/// Asks the socket which build is on the other end.
///
/// `version` is what this build reports for itself — the same string the
/// daemon answers a `Ping` with, so two builds differ exactly when their
/// commits do.
///
/// **Nothing is spawned.** A machine with no daemon running has nothing to
/// restart, and starting one to discover that would be the opposite of
/// what the caller is asking about.
pub async fn daemon(client: &Client, version: &str) -> Daemon {
    let Ok(mut connection) = client.connect().await else {
        return Daemon::Stopped;
    };

    match connection.handshake().await {
        Ok(pong) if pong.version == version => Daemon::Current,
        Ok(_) => Daemon::Previous,
        // A refused handshake is a daemon speaking a protocol this build
        // does not, which is the same answer by a shorter route.
        Err(_) => Daemon::Previous,
    }
}

/// The same question, from inside the build being replaced.
///
/// Its version is not asked for and could not be used: this process was
/// started from a binary that has just been overwritten, so what it calls
/// itself says nothing about what has landed. Anything still answering is
/// the build the update replaced, by construction.
pub async fn daemon_after_replacing(client: &Client) -> Daemon {
    match client.connect().await {
        Ok(_) => Daemon::Previous,
        Err(_) => Daemon::Stopped,
    }
}

/// Everything this build can work out about the machine it is running on.
///
/// `repository` is the one the command ran in, when it ran in one at all.
pub fn steps(daemon: Daemon, repository: Option<&Path>) -> Vec<Step> {
    let mut steps = Vec::new();

    // Most first, least last: a daemon from another build decides what
    // every command does next, an old plist decides whether the URLs
    // answer, and the Skill is read by an agent tomorrow.
    steps.extend(daemon_step(daemon, minato_core::launchd::is_installed()));
    steps.extend(setup_step(installed_plist().as_deref()));
    steps.extend(repository.and_then(skill_step));

    steps
}

/// What an update can still be sure of, from inside the build being
/// replaced.
///
/// **Only the daemon.** The other two compare a file on this machine with
/// what the running binary carries, and the running binary is the one that
/// has just been superseded — so both would answer for the build being
/// left behind rather than the one that has landed. The build that lands
/// says the rest on its first run, where it can compare against itself.
pub fn steps_after_replacing(daemon: Daemon) -> Vec<Step> {
    daemon_step(daemon, minato_core::launchd::is_installed())
        .into_iter()
        .collect()
}

/// Replacing a daemon left over from another build.
///
/// With launchd holding the job, stopping is the whole of it: a clean exit
/// is what makes launchd start the job again, and it starts it from the
/// binary that is there now. Without launchd nothing would bring it back,
/// so it is stopped and started here.
fn daemon_step(daemon: Daemon, launchd: bool) -> Option<Step> {
    if daemon != Daemon::Previous {
        return None;
    }

    Some(Step::new(
        "the daemon is still the previous build",
        if launchd {
            "minato daemon stop"
        } else {
            "minato daemon restart"
        },
    ))
}

/// `minato setup`, when the installed plist predates the shape this build
/// writes.
///
/// A plist with no revision in it says nothing: it was written before the
/// marker existed, and calling it out would be a guess about a file that
/// is most likely exactly right.
fn setup_step(installed: Option<&str>) -> Option<Step> {
    let revision = installed.and_then(crate::launchd::revision_of)?;

    (revision < crate::launchd::PLIST_REVISION)
        .then(|| Step::new("the LaunchDaemon is an older build's", "minato setup"))
}

/// The plist launchd has, when there is one to read.
fn installed_plist() -> Option<String> {
    if !minato_core::launchd::is_installed() {
        return None;
    }

    std::fs::read_to_string(minato_core::launchd::plist_path()).ok()
}

/// `minato skill install --force`, when the copy in this repository is not
/// the one this build carries.
///
/// **Only where there is one already.** The Skill is opt-in, and a
/// repository that has never had it is not missing anything.
///
/// Why it differs is not asked: an update is the ordinary reason, a local
/// edit is the other one, and `--force` is the only command that settles
/// either. The reason line says whose copy is about to win, which is the
/// part someone with edits needs to read.
fn skill_step(repository: &Path) -> Option<Step> {
    let installed = std::fs::read_to_string(crate::skill::path_in(repository)).ok()?;

    (installed != crate::skill::contents()).then(|| {
        Step::new(
            "this repository's Skill is not this build's",
            "minato skill install --force",
        )
    })
}

// ---------------------------------------------------------------------------
// Noticing that the build has changed
// ---------------------------------------------------------------------------

/// The build the machine last ran.
#[derive(Debug, Serialize, Deserialize)]
struct Record {
    commit: String,
}

fn record_path(paths: &minato_core::Paths) -> PathBuf {
    paths.root().join("build.json")
}

/// Whether this build is one the machine has not run before, remembering
/// it either way.
///
/// This is what covers every way of updating that is not `minato update` —
/// `install.sh` again, a package manager, a `cargo install` — none of which
/// can print anything about Minato's own state.
///
/// **`false` when nothing has been recorded yet.** A machine with no record
/// has just installed Minato, and a first installation is not an update:
/// there is no previous daemon and no older plist, only a `minato setup`
/// that has yet to be run and says so itself.
///
/// Best effort with the file: a record that cannot be written costs a
/// repeated notice, which is worth less than a message about a file nobody
/// asked for.
pub fn is_new_build(paths: &minato_core::Paths) -> bool {
    changed(&record_path(paths), minato_core::BUILD_COMMIT)
}

/// The whole of the rule, with the build to compare against passed in.
///
/// Split out so it can be tested at all: which commit this binary was
/// built from is decided by the checkout it was built in, and a test that
/// depended on it would pass or fail on how the tree was fetched.
fn changed(path: &Path, commit: &str) -> bool {
    // Two builds with no commit between them cannot be told apart, and
    // "you have updated" would be a guess made once per run.
    if commit == crate::update::NO_COMMIT {
        return false;
    }

    let previous = read_record(path);
    write_record(path, commit);

    matches!(previous, Some(recorded) if recorded != commit)
}

fn read_record(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let record: Record = serde_json::from_str(&text).ok()?;
    Some(record.commit)
}

fn write_record(path: &Path, commit: &str) {
    if let Ok(text) = serde_json::to_string(&Record {
        commit: commit.to_string(),
    }) {
        let _ = std::fs::write(path, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_daemon_from_another_build_is_worth_a_step() {
        // Nothing running, or this build already running: there is
        // nothing to replace, and saying so anyway would put a line under
        // every update that ever lands.
        assert_eq!(daemon_step(Daemon::Stopped, false), None);
        assert_eq!(daemon_step(Daemon::Current, false), None);
        assert!(daemon_step(Daemon::Previous, false).is_some());
    }

    #[test]
    fn launchd_is_told_to_stop_rather_than_restart() {
        // Restarting by hand where launchd owns the job starts a daemon
        // outside it, which is exactly the state that leaves 80 and 443
        // unheld.
        let with = daemon_step(Daemon::Previous, true).expect("a step");
        assert_eq!(with.command, "minato daemon stop");

        let without = daemon_step(Daemon::Previous, false).expect("a step");
        assert_eq!(without.command, "minato daemon restart");
    }

    #[test]
    fn an_update_speaks_only_for_the_daemon() {
        // The rest is compared against what this binary carries, and this
        // binary is the one being replaced.
        let steps = steps_after_replacing(Daemon::Previous);

        assert_eq!(steps.len(), 1, "got: {steps:?}");
        assert!(steps[0].command.starts_with("minato daemon"));

        assert!(steps_after_replacing(Daemon::Stopped).is_empty());
    }

    #[test]
    fn an_older_plist_asks_for_setup() {
        let older = format!("<!-- minato plist revision {} -->", 0);
        let step = setup_step(Some(&older)).expect("a step");

        assert_eq!(step.command, "minato setup");
    }

    #[test]
    fn a_current_plist_says_nothing() {
        let current = format!(
            "<!-- minato plist revision {} -->",
            crate::launchd::PLIST_REVISION
        );

        assert_eq!(setup_step(Some(&current)), None);
    }

    #[test]
    fn a_plist_with_no_revision_says_nothing() {
        // Written before the marker existed. It is most likely the right
        // shape, and "run setup again" is a privileged, interactive
        // command to be sent off to on a hunch.
        assert_eq!(setup_step(Some("<plist version=\"1.0\"></plist>")), None);
        assert_eq!(setup_step(None), None);
    }

    #[test]
    fn a_skill_matching_this_build_says_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        crate::skill::install(dir.path(), false).expect("installs");

        assert_eq!(skill_step(dir.path()), None);
    }

    #[test]
    fn a_skill_from_elsewhere_asks_to_be_replaced() {
        let dir = tempfile::tempdir().expect("tempdir");
        crate::skill::install(dir.path(), false).expect("installs");
        std::fs::write(crate::skill::path_in(dir.path()), "another build's").expect("writes");

        let step = skill_step(dir.path()).expect("a step");

        // Without --force it refuses, so anything else would be advice
        // that cannot be followed.
        assert_eq!(step.command, "minato skill install --force");
    }

    #[test]
    fn a_repository_without_the_skill_is_left_alone() {
        // It is opt-in. Suggesting it here would be advertising, not a
        // follow-up.
        let dir = tempfile::tempdir().expect("tempdir");

        assert_eq!(skill_step(dir.path()), None);
    }

    /// Two commits that differ, at the length the record holds.
    const ONE: &str = "c7282b8530f6408ba5048b2721e24d7cb33425b0";
    const TWO: &str = "56a3859f1d0a4e4b9c7f2e6d8a1b3c5d7e9f0a12";

    #[test]
    fn the_first_build_a_machine_sees_is_not_an_update() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("build.json");

        assert!(!changed(&path, ONE), "a fresh installation has no steps");
        assert!(path.is_file(), "it is still what ran here last");
    }

    #[test]
    fn a_build_is_new_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("build.json");

        // Another build ran here first.
        write_record(&path, TWO);

        assert!(changed(&path, ONE));
        assert!(
            !changed(&path, ONE),
            "the notice belongs to the run that found it, not to every run after"
        );
    }

    #[test]
    fn a_build_with_no_commit_notices_nothing() {
        // A source tarball, or a build from before the commit was
        // recorded: every run of it looks exactly like every other.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("build.json");
        write_record(&path, ONE);

        assert!(!changed(&path, crate::update::NO_COMMIT));
    }

    #[test]
    fn a_record_that_cannot_be_read_is_a_first_run() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("build.json");
        std::fs::write(&path, "{ not json").expect("writes");

        assert!(!changed(&path, ONE), "nothing to compare against");
        assert_eq!(read_record(&path).as_deref(), Some(ONE), "and it is fixed");
    }
}
