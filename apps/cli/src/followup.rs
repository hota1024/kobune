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
use std::time::Duration;

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
    /// Nothing is — or nothing that could be asked in the moment this was
    /// worth spending on it. Either way there is nothing to say about it.
    Stopped,
    /// A daemon from this build.
    Current,
    /// A daemon from a build that is not this one. **Which way round is
    /// not known**: a daemon started by hand from a newer binary lands
    /// here too, and the answer — restart it — is the same.
    Other,
}

/// How long the socket gets to answer.
///
/// This runs after a command that has already printed its result, so the
/// cost of asking has to round to nothing. A daemon wedged behind
/// something slow is reported as [`Daemon::Stopped`]: not a claim that it
/// is down, a refusal to hold up a finished command over a remark.
const PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// Asks the socket which build is on the other end.
///
/// `version` is what this build reports for itself. The daemon answers a
/// `Ping` with the same string built the same way ([`minatod::version`]),
/// and the two binaries carry one crate version between them and are only
/// ever installed as a pair — so they differ exactly when their commits
/// do.
///
/// **Nothing is spawned.** A machine with no daemon running has nothing to
/// restart, and starting one to discover that would be the opposite of
/// what the caller is asking about.
pub async fn daemon(client: &Client, version: &str) -> Daemon {
    tokio::time::timeout(PROBE_TIMEOUT, ask(client, version))
        .await
        .unwrap_or(Daemon::Stopped)
}

async fn ask(client: &Client, version: &str) -> Daemon {
    let Ok(mut connection) = client.connect().await else {
        return Daemon::Stopped;
    };

    match connection.handshake().await {
        Ok(pong) if pong.version == version => Daemon::Current,
        Ok(_) => Daemon::Other,
        // A refused handshake is a daemon speaking a protocol this build
        // does not, which is the same answer by a shorter route.
        Err(_) => Daemon::Other,
    }
}

/// The same question, from inside the build being replaced.
///
/// Its version is not asked for and could not be used: this process was
/// started from a binary that has just been overwritten, so what it calls
/// itself says nothing about what has landed. Anything still answering is
/// the build the update replaced, by construction.
pub async fn daemon_after_replacing(client: &Client) -> Daemon {
    let connected = tokio::time::timeout(PROBE_TIMEOUT, client.connect())
        .await
        .is_ok_and(|connection| connection.is_ok());

    if connected {
        Daemon::Other
    } else {
        Daemon::Stopped
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
    if daemon == Daemon::Other {
        // Not "the previous build": all that was established is that it
        // is not this one, and a daemon someone started by hand from a
        // newer binary would make the stronger claim a false one.
        steps.push(daemon_step("the daemon is not this build"));
    }

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
    if daemon != Daemon::Other {
        return Vec::new();
    }

    // Here the stronger wording is a fact: the binaries were replaced a
    // moment ago, so a process still on the socket predates them.
    vec![daemon_step("the daemon is still the previous build")]
}

/// Replacing a daemon left over from another build.
///
/// **The same command wherever it runs.** `restart` starts the daemon back
/// up the way every other command does, and that path asks launchd first
/// where launchd has the job ([`minato_client::Client::connect_or_spawn`]),
/// so the process it ends with is one launchd started — holding 80 and 443,
/// running the binary that is there now.
///
/// `stop` alone would do on a launchd machine, and used to be what this
/// said. It is worse in two ways: a clean exit is not restarted
/// (`KeepAlive { SuccessfulExit: false }`), so the daemon stays down until
/// something arrives on a port to demand-launch it and `minato daemon
/// status` reports it stopped in the meantime — and it made the advice
/// depend on a plist, so `--json` carried one of two commands for one
/// state. `restart` is also what the CLI already says when it meets an old
/// daemon head-on ([`minato_client::ClientError::VersionMismatch`]).
fn daemon_step(reason: &str) -> Step {
    Step::new(reason, minato_core::launchd::RESTART_COMMAND)
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
///
/// No `is_installed` first: it is that same file being there, and reading
/// a file that is not answers the question in one call.
fn installed_plist() -> Option<String> {
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

/// Whether this build is one the machine has not run before.
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
/// It does not record anything. [`remember`] does, and the caller runs it
/// *after* the steps are printed — a probe that was interrupted in between
/// would otherwise have marked the build as seen without anyone seeing it.
pub fn is_new_build(paths: &minato_core::Paths) -> bool {
    changed(&record_path(paths), minato_core::BUILD_COMMIT)
}

/// Writes down that this build has now run here.
///
/// Best effort: a record that cannot be written costs a repeated notice,
/// which is worth less than a message about a file nobody asked for.
pub fn remember(paths: &minato_core::Paths) {
    if minato_core::BUILD_COMMIT == crate::update::NO_COMMIT {
        return;
    }

    write_record(&record_path(paths), minato_core::BUILD_COMMIT);
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

    matches!(read_record(path), Some(recorded) if recorded != commit)
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
        assert!(steps_after_replacing(Daemon::Stopped).is_empty());
        assert!(steps_after_replacing(Daemon::Current).is_empty());
        assert!(!steps_after_replacing(Daemon::Other).is_empty());
    }

    #[test]
    fn both_paths_answer_an_older_daemon_with_a_restart() {
        // Through the entry points rather than `daemon_step`, which now
        // returns a constant: what is worth pinning is the command that
        // reaches a notice and a `--json` document.
        //
        // Both machines, with a LaunchDaemon and without, is what the
        // *absence* of a branch in `daemon_step` gives — reading the real
        // `/Library/LaunchDaemons` here would only test the host this runs
        // on, so it is not claimed.
        assert_eq!(
            steps_after_replacing(Daemon::Other)[0].command,
            minato_core::launchd::RESTART_COMMAND
        );
        assert_eq!(
            steps(Daemon::Other, None)[0].command,
            minato_core::launchd::RESTART_COMMAND
        );
    }

    #[test]
    fn every_step_names_a_command_that_parses() {
        // These reach a person as a notice and an agent as `--json`
        // `next`, and an agent runs what it is given. A command that has
        // been renamed out from under one of them fails at the prompt,
        // which is how `minato daemon restart` was once advised before it
        // existed.
        //
        // **Each step is built from an input that produces it**, not from
        // whatever this machine happens to have. Through `steps` the setup
        // and skill steps are `None` on any machine without an outdated
        // plist and a stale Skill — which is every machine in CI — so the
        // loop would run over one command and prove nothing about the
        // other two.
        let repository = tempfile::tempdir().expect("tempdir");
        let stale_skill = crate::skill::path_in(repository.path());
        std::fs::create_dir_all(stale_skill.parent().expect("has a parent")).expect("creates");
        std::fs::write(&stale_skill, "not this build's Skill").expect("writes");

        let older_plist = format!("<!-- minato plist revision {} -->", 0);

        let steps = [
            Some(daemon_step("the daemon is not this build")),
            setup_step(Some(&older_plist)),
            skill_step(repository.path()),
        ];

        let mut checked = Vec::new();
        for step in steps.into_iter().flatten() {
            use clap::Parser;

            crate::Cli::try_parse_from(step.command.split_whitespace())
                .unwrap_or_else(|err| panic!("`{}` does not parse: {err}", step.command));
            checked.push(step.command);
        }

        assert_eq!(checked.len(), 3, "every step was built: {checked:?}");
    }

    #[test]
    fn an_update_claims_the_daemon_is_the_previous_build_and_a_run_does_not() {
        // After the swap it is a fact — the binaries went a moment ago, so
        // a process still answering predates them. On an ordinary run all
        // that was established is that it is not this build, and a daemon
        // started by hand from a newer one would make the stronger claim
        // false.
        let replaced = steps_after_replacing(Daemon::Other);
        assert_eq!(replaced[0].reason, "the daemon is still the previous build");

        let running = steps(Daemon::Other, None);
        assert_eq!(running[0].reason, "the daemon is not this build");
    }

    #[test]
    fn an_update_speaks_only_for_the_daemon() {
        // The rest is compared against what this binary carries, and this
        // binary is the one being replaced.
        let steps = steps_after_replacing(Daemon::Other);

        assert_eq!(steps.len(), 1, "got: {steps:?}");
        assert!(steps[0].command.starts_with("minato daemon"));
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
    }

    #[test]
    fn a_build_is_new_until_it_is_remembered() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("build.json");

        // Another build ran here first.
        write_record(&path, TWO);

        assert!(changed(&path, ONE));
        assert!(
            changed(&path, ONE),
            "asking is not what settles it — a run that never got to print \
             its steps has to find them again"
        );

        write_record(&path, ONE);
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

        write_record(&path, ONE);
        assert_eq!(read_record(&path).as_deref(), Some(ONE), "and it is fixed");
    }
}
