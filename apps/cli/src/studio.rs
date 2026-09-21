//! `kobune studio` — handing over to the dashboard's own binary.
//!
//! The studio is a separate program for the reason `kobuned` is: one
//! binary per job, and the page it serves is four hundred kilobytes that
//! `kobune ls` has no use for. This subcommand exists so it is
//! discoverable from the CLI anyway, and it is a handover rather than a
//! wrapper — `exec` replaces this process, so signals, the terminal and
//! the exit code are the studio's from that point on and there is no
//! parent left to get them wrong.

use std::path::PathBuf;
use std::process::Command;

use crate::CliError;

/// The binary to hand over to.
const STUDIO_PROGRAM: &str = "kobune-studio";

/// Overrides the lookup, the way `KOBUNE_DAEMON` does for the daemon.
const STUDIO_ENV: &str = "KOBUNE_STUDIO";

/// Replaces this process with the studio.
///
/// Returns only on failure: on success there is nothing left to return to.
pub fn exec(
    port: Option<u16>,
    no_open: bool,
    workspace: Option<&str>,
) -> Result<std::convert::Infallible, CliError> {
    use std::os::unix::process::CommandExt as _;

    let program = resolve();

    let mut command = Command::new(&program);
    if let Some(port) = port {
        command.arg("--port").arg(port.to_string());
    }
    if no_open {
        command.arg("--no-open");
    }
    // `kobune tui -w` opens that workspace. The same flag on the same
    // screen in a browser does the same thing, or it is a flag that looks
    // accepted and is not.
    if let Some(workspace) = workspace {
        command.arg("--workspace").arg(workspace);
    }

    // The working directory is inherited, and the studio resolves the
    // project from it exactly as every other command does. Passing
    // `--path` would be this process deciding on the studio's behalf.
    let err = command.exec();

    // **A missing sibling is the expected failure, not a surprise.** An
    // installation updated by a `kobune` that predates the studio has the
    // new CLI and no studio beside it, because that older `update` did not
    // know to extract one. Saying so, with the command that fixes it,
    // is the difference between a puzzle and an instruction.
    if err.kind() == std::io::ErrorKind::NotFound {
        return Err(CliError::Local(format!(
            "cannot find {STUDIO_PROGRAM}. It ships beside kobune, so an \
             installation updated before the studio existed will not have \
             it yet.\n\nRun `kobune update` again, or reinstall with \
             `curl -fsSL https://kobune.1024.works/install.sh | sh`."
        )));
    }

    Err(CliError::Local(format!(
        "cannot start {}: {err}",
        program.display()
    )))
}

/// Where the studio is, preferring the copy that shipped with this binary.
///
/// The same order the client uses to find the daemon: an override, then
/// next door, then `PATH`. Next door before `PATH` so a development build
/// in `target/debug` wins over an installed one.
fn resolve() -> PathBuf {
    if let Some(value) = std::env::var_os(STUDIO_ENV) {
        return PathBuf::from(value);
    }

    if let Ok(current) = std::env::current_exe()
        && let Some(dir) = current.parent()
    {
        let sibling = dir.join(STUDIO_PROGRAM);
        if sibling.is_file() {
            return sibling;
        }
    }

    PathBuf::from(STUDIO_PROGRAM)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **One test, because the environment is process-global.** Two of
    /// these would run on different threads against the same variable, and
    /// the one that read it could find the other's cleanup instead — the
    /// kind of race `docs/DESIGN.md` notes only ever fails under Linux's
    /// scheduling, long after it was written.
    #[test]
    fn the_override_wins_and_the_fallback_is_the_bare_name() {
        // SAFETY: the variable is set and read back within this test, and
        // nothing else in this binary touches it.
        unsafe { std::env::set_var(STUDIO_ENV, "/somewhere/else/kobune-studio") };
        assert_eq!(resolve(), PathBuf::from("/somewhere/else/kobune-studio"));

        // The test binary lives in target/debug/deps, where no studio is
        // ever installed, so removing the override reaches the last branch.
        unsafe { std::env::remove_var(STUDIO_ENV) };
        assert_eq!(resolve(), PathBuf::from(STUDIO_PROGRAM));
    }
}
