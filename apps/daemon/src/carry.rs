//! Bringing declared files into a worktree.
//!
//! `git worktree add` gives a new worktree the tracked files and nothing
//! else, so a file that is untracked but required — `.env` in nearly every
//! project — is simply absent, and the environment fails to start every
//! time. `[project] carry` names those files; this copies them.
//!
//! **A `kobune.toml` arrives with a clone as readily as it is written by
//! hand**, so both ends are treated as untrusted: the source must resolve
//! inside the repository, and the destination inside the worktree. The
//! second is not the same check as the first — `git worktree add` has just
//! written whatever the branch says, and a branch can track a symlink.

use std::path::Path;

use kobune_runtime::EventSink;

use crate::paths::{Containment, resolve_within};

const STEP: &str = "carry";
const LABEL: &str = "carrying files over";

/// Copies the declared files into a worktree.
///
/// **Nothing here fails the caller.** The worktree already exists by this
/// point, and a `.env` that was never there is not a reason to leave
/// someone with a half-made one. What is skipped is said out loud instead,
/// so it is visible rather than silently missing.
///
/// `announce` is false where this runs on every start: a step that reports
/// "nothing to copy" on each `kobune up` is noise, while one that reports a
/// file appearing is worth seeing.
pub fn files(
    entries: &[String],
    main_root: &Path,
    worktree: &Path,
    announce: bool,
    events: &EventSink,
) {
    if entries.is_empty() {
        return;
    }

    if announce {
        events.step_started(STEP, LABEL);
    }

    let mut carried = 0usize;
    let mut failed = 0usize;

    for entry in entries {
        match one(entry, main_root, worktree) {
            Ok(true) => carried += 1,
            Ok(false) => {}
            Err(reason) => {
                failed += 1;
                events.warn(format!("cannot carry `{entry}`: {reason}"));
            }
        }
    }

    if !announce {
        // Quiet unless something actually appeared. A file arriving at
        // start-up is a change worth a line; the usual nothing is not.
        if carried > 0 {
            events.step_started(STEP, LABEL);
            events.step_done(STEP, format!("{LABEL} ({carried})"));
        }
        return;
    }

    // **A failure is not a skip.** Reporting "nothing to copy" over entries
    // that could not be copied contradicts the warnings just emitted, and
    // the services are about to start without a file they need.
    if failed > 0 {
        events.step_failed(
            STEP,
            LABEL,
            format!("{failed} of {} could not be carried", entries.len()),
        );
        return;
    }

    match carried {
        0 => events.step_skipped(STEP, LABEL, "nothing to copy"),
        n => events.step_done(STEP, format!("{LABEL} ({n})")),
    }
}

/// Copies one entry. `Ok(false)` means there was nothing to do.
fn one(entry: &str, main_root: &Path, worktree: &Path) -> Result<bool, String> {
    let source = main_root.join(entry);

    // Not every checkout has one yet, and refusing to finish a worktree
    // over a file the user never created would be worse than the gap it
    // fills.
    if !source.exists() {
        return Ok(false);
    }

    let resolved = match resolve_within(main_root, &source) {
        Ok(resolved) => resolved,
        Err(Containment::Unresolvable(err)) => {
            return Err(format!("cannot resolve {}: {err}", source.display()));
        }
        Err(Containment::Outside(landed)) => {
            return Err(format!(
                "it resolves to {}, which is outside the repository",
                landed.display()
            ));
        }
    };

    if !resolved.is_file() {
        return Err("only files are carried, and this is not one".to_string());
    }

    let destination = worktree.join(entry);

    // **Before anything is resolved.** Anything git just checked out wins:
    // this is for what git does not carry, not a way to replace what it
    // does. Standing down here also settles the dangling symlink — the one
    // whose target is where a secret would be written — without having to
    // reason about where it points.
    //
    // `symlink_metadata` rather than `exists`, which follows the link and
    // reports the absence of its target as the absence of the link.
    if destination.symlink_metadata().is_ok() {
        return Ok(false);
    }

    contained_destination(&destination, worktree)?;

    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("cannot make {}: {err}", parent.display()))?;
    }

    copy_file(&resolved, &destination)
        .map_err(|err| format!("cannot copy to {}: {err}", destination.display()))
        .map(|()| true)
}

/// Refuses a destination that would land outside the worktree.
///
/// **The source check does not cover this.** That one constrains what is
/// read, in the main checkout; this constrains what is written, in a
/// worktree `git worktree add` has just filled from the branch. A tracked
/// `apps -> ~/Library/LaunchAgents` would otherwise turn a carry into an
/// arbitrary file write with contents the repository chose.
///
/// The path does not exist yet, so what is resolved is its **deepest
/// existing ancestor**: everything below that is about to be created and
/// cannot be a symlink, and anything at or above it that leaves is caught.
fn contained_destination(destination: &Path, worktree: &Path) -> Result<(), String> {
    let mut anchor = destination;

    // `symlink_metadata`, so a dangling symlink counts as present. `exists`
    // follows it, reports false, and hands back the very path worth
    // refusing.
    while anchor.symlink_metadata().is_err() {
        anchor = anchor
            .parent()
            .ok_or_else(|| "it does not sit under the worktree".to_string())?;
    }

    match resolve_within(worktree, anchor) {
        Ok(_) => Ok(()),
        Err(Containment::Unresolvable(err)) => {
            Err(format!("cannot resolve {}: {err}", anchor.display()))
        }
        Err(Containment::Outside(landed)) => Err(format!(
            "it would write to {}, which is outside the worktree",
            landed.display()
        )),
    }
}

/// Copies a file, refusing to follow or replace anything already there.
///
/// `create_new` rather than [`std::fs::copy`]: `copy` follows a symlink at
/// the destination, while `O_EXCL` refuses one even when it dangles. That
/// is what stops a tracked symlink from redirecting the write.
///
/// The mode comes from the source, so a `0600` `.env` stays `0600` instead
/// of being widened on the way over.
fn copy_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    let mut reader = std::fs::File::open(source)?;
    let metadata = reader.metadata()?;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        options.mode(metadata.permissions().mode());
    }

    let mut writer = options.open(destination)?;
    std::io::copy(&mut reader, &mut writer)?;

    // Set again explicitly: `mode` at creation is masked by the umask, and
    // the promise is that the permissions come across unchanged.
    writer.set_permissions(metadata.permissions())?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kobune_api::Event;
    use std::path::PathBuf;

    /// A main checkout and an empty worktree beside it.
    fn dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let main = dir.path().join("repo");
        let worktree = dir.path().join("repo.wt/feat-1");

        std::fs::create_dir_all(&main).expect("creates");
        std::fs::create_dir_all(&worktree).expect("creates");

        (dir, main, worktree)
    }

    /// Runs `files` and returns what it emitted.
    fn emitted(entries: &[String], main: &Path, worktree: &Path, announce: bool) -> Vec<Event> {
        let (events, mut rx) = EventSink::channel();
        files(entries, main, worktree, announce, &events);
        drop(events);

        let mut seen = Vec::new();
        while let Ok(event) = rx.try_recv() {
            seen.push(event);
        }
        seen
    }

    fn entries(of: &[&str]) -> Vec<String> {
        of.iter().map(|e| e.to_string()).collect()
    }

    #[test]
    fn carries_a_file_git_leaves_behind() {
        // The whole point: `git worktree add` brings the tracked files and
        // nothing else, so an untracked .env is simply absent.
        let (_dir, main, worktree) = dirs();
        std::fs::write(main.join(".env"), "SECRET=1\n").expect("writes");

        assert_eq!(one(".env", &main, &worktree), Ok(true));
        assert_eq!(
            std::fs::read_to_string(worktree.join(".env")).expect("carried"),
            "SECRET=1\n"
        );
    }

    #[test]
    fn carries_into_a_directory_that_does_not_exist_yet() {
        let (_dir, main, worktree) = dirs();
        std::fs::create_dir_all(main.join("apps/api")).expect("creates");
        std::fs::write(main.join("apps/api/.dev.vars"), "K=v\n").expect("writes");

        assert_eq!(one("apps/api/.dev.vars", &main, &worktree), Ok(true));
        assert!(worktree.join("apps/api/.dev.vars").is_file());
    }

    #[test]
    fn a_missing_source_is_not_a_failure() {
        // Not every checkout has a .env yet, and refusing to finish a
        // worktree over one would be worse than the gap it fills.
        let (_dir, main, worktree) = dirs();

        assert_eq!(one(".env", &main, &worktree), Ok(false));
        assert!(!worktree.join(".env").exists());
    }

    #[test]
    fn what_git_checked_out_is_never_overwritten() {
        let (_dir, main, worktree) = dirs();
        std::fs::write(main.join(".env"), "from main\n").expect("writes");
        std::fs::write(worktree.join(".env"), "from git\n").expect("writes");

        assert_eq!(one(".env", &main, &worktree), Ok(false));
        assert_eq!(
            std::fs::read_to_string(worktree.join(".env")).expect("kept"),
            "from git\n",
            "the carry is for what git does not carry"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_source_symlink_out_of_the_repository_is_refused() {
        // The syntax check in the config cannot see this one: the entry is
        // a plain relative path, and only the resolved target leaves.
        let (dir, main, worktree) = dirs();
        let outside = dir.path().join("secrets");
        std::fs::write(&outside, "not yours\n").expect("writes");
        std::os::unix::fs::symlink(&outside, main.join(".env")).expect("links");

        let err = one(".env", &main, &worktree).unwrap_err();
        assert!(err.contains("outside the repository"), "{err}");
        assert!(!worktree.join(".env").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_tracked_symlinked_directory_cannot_redirect_the_write() {
        // `git worktree add` has already written whatever the branch says.
        // A branch that tracks `apps -> somewhere else` would otherwise
        // make this an arbitrary file write with contents it chose.
        let (dir, main, worktree) = dirs();
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).expect("creates");

        std::fs::create_dir_all(main.join("apps")).expect("creates");
        std::fs::write(main.join("apps/agent.plist"), "payload\n").expect("writes");
        std::os::unix::fs::symlink(&elsewhere, worktree.join("apps")).expect("links");

        let err = one("apps/agent.plist", &main, &worktree).unwrap_err();
        assert!(err.contains("outside the worktree"), "{err}");
        assert!(
            !elsewhere.join("agent.plist").exists(),
            "nothing may be written through the link"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_at_the_destination_is_not_written_through() {
        // `exists` follows it and says no, which would otherwise read as
        // "there is nothing there, go ahead".
        let (dir, main, worktree) = dirs();
        let target = dir.path().join("stolen");
        std::fs::write(main.join(".env"), "SECRET=1\n").expect("writes");
        std::os::unix::fs::symlink(&target, worktree.join(".env")).expect("links");

        assert_eq!(
            one(".env", &main, &worktree),
            Ok(false),
            "something is already there, whatever it points at"
        );
        assert!(!target.exists(), "the secret must not follow the link");
    }

    #[cfg(unix)]
    #[test]
    fn a_carried_file_keeps_its_permissions() {
        use std::os::unix::fs::PermissionsExt;

        // A .env holds secrets. Widening it on the way over would be a
        // quiet downgrade.
        let (_dir, main, worktree) = dirs();
        let source = main.join(".env");
        std::fs::write(&source, "SECRET=1\n").expect("writes");
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).expect("chmod");

        assert_eq!(one(".env", &main, &worktree), Ok(true));

        let mode = std::fs::metadata(worktree.join(".env"))
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn a_directory_is_not_carried() {
        let (_dir, main, worktree) = dirs();
        std::fs::create_dir_all(main.join("config")).expect("creates");

        let err = one("config", &main, &worktree).unwrap_err();
        assert!(err.contains("only files"), "{err}");
    }

    #[test]
    fn nothing_declared_says_nothing() {
        // Otherwise every `kobune new` grows a step for a feature the
        // project does not use.
        let (_dir, main, worktree) = dirs();

        assert!(emitted(&[], &main, &worktree, true).is_empty());
    }

    #[test]
    fn a_successful_carry_is_reported_with_its_count() {
        let (_dir, main, worktree) = dirs();
        std::fs::write(main.join(".env"), "a\n").expect("writes");
        std::fs::write(main.join(".env.local"), "b\n").expect("writes");

        let events = emitted(&entries(&[".env", ".env.local"]), &main, &worktree, true);
        let labels: Vec<String> = events
            .iter()
            .filter_map(|event| match event {
                Event::Step { label, .. } => Some(label.clone()),
                _ => None,
            })
            .collect();

        assert!(labels.iter().any(|l| l.contains("(2)")), "{labels:?}");
    }

    #[test]
    fn a_failure_is_not_reported_as_a_skip() {
        // "nothing to copy" alongside a warning saying otherwise is how a
        // missing file gets past someone reading the output.
        let (dir, main, worktree) = dirs();
        let outside = dir.path().join("secrets");
        std::fs::write(&outside, "x\n").expect("writes");

        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, main.join(".env")).expect("links");
        #[cfg(not(unix))]
        std::fs::write(main.join(".env"), "x\n").expect("writes");

        let events = emitted(&entries(&[".env"]), &main, &worktree, true);

        let warned = events.iter().any(
            |event| matches!(event, Event::Log { message, .. } if message.contains("cannot carry")),
        );
        let skipped = events.iter().any(|event| {
            matches!(
                event,
                Event::Step {
                    status: kobune_api::StepStatus::Skipped { .. },
                    ..
                }
            )
        });

        #[cfg(unix)]
        {
            assert!(warned, "the reason has to be said: {events:?}");
            assert!(!skipped, "a failure is not a skip: {events:?}");
        }
        #[cfg(not(unix))]
        let _ = (warned, skipped);
    }

    #[test]
    fn a_quiet_run_says_nothing_when_there_was_nothing_to_do() {
        // `kobune up` happens constantly. A step reporting "nothing to
        // copy" every time trains people to skim past the output.
        let (_dir, main, worktree) = dirs();
        std::fs::write(main.join(".env"), "a\n").expect("writes");
        std::fs::write(worktree.join(".env"), "a\n").expect("writes");

        assert!(emitted(&entries(&[".env"]), &main, &worktree, false).is_empty());
    }

    #[test]
    fn a_quiet_run_still_says_when_a_file_appears() {
        // Adding `carry` to a project whose worktrees already exist is the
        // case this covers, and a file arriving is worth a line.
        let (_dir, main, worktree) = dirs();
        std::fs::write(main.join(".env"), "a\n").expect("writes");

        let events = emitted(&entries(&[".env"]), &main, &worktree, false);
        assert!(!events.is_empty(), "a file appeared and nothing said so");
    }
}
