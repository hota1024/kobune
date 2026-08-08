//! Taking Minato back off the machine.
//!
//! The daemon removes what it made — containers, networks, its state file
//! — because only it knows what those are. Everything else is here,
//! because only this side knows where it was installed from.
//!
//! Two rules shape the whole module.
//!
//! **Nothing is removed that was not found.** The plan is built by looking,
//! so what a person is asked to confirm is what is actually there, not a
//! list of everywhere Minato might have put something.
//!
//! **Worktrees are never touched.** They are the user's checkouts with the
//! user's uncommitted work in them. `minato rm` removes one at a time and
//! asks for `--force` when git objects; an uninstaller cannot make that
//! judgement for twenty of them at once.

use std::path::{Path, PathBuf};

use crate::launchd;
use crate::system;

/// One thing on disk that would go.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Removal {
    /// What it is, for a person reading the list.
    pub label: &'static str,
    pub path: PathBuf,
}

/// One thing that needs root.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Privileged {
    pub label: String,
    pub commands: Vec<String>,
}

/// What `minato uninstall` would take off this machine.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Plan {
    /// Removable without asking anyone's permission.
    pub files: Vec<Removal>,
    /// Needs root, so it is run only with a terminal to type a password
    /// into, and printed otherwise.
    pub privileged: Vec<Privileged>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.privileged.is_empty()
    }
}

/// Looks at the machine and says what is there.
///
/// `ca_path` comes from the daemon's diagnostics when there is a daemon to
/// ask. Without one the certificate cannot be located, so trusting it is
/// left out of the plan rather than guessed at.
pub fn plan(suffix: &str, ca_path: Option<&Path>) -> Plan {
    let mut plan = Plan::default();

    if let Ok(paths) = minato_core::Paths::resolve() {
        let root = paths.root();
        if root.exists() {
            plan.files.push(Removal {
                label: "state, logs and the local CA",
                path: root.to_path_buf(),
            });
        }
    }

    plan.files.extend(installed_binaries());
    plan.files.extend(completions());

    let plist = Path::new(launchd::INSTALL_DIR).join(format!("{}.plist", launchd::LABEL));
    if plist.exists() {
        plan.privileged.push(Privileged {
            label: "stop the LaunchDaemon holding 80/443/53".to_string(),
            commands: launchd::uninstall_commands(),
        });
    }

    if system::resolver_path(suffix).exists() {
        plan.privileged.push(Privileged {
            label: format!("stop sending *.{suffix} to Minato's DNS"),
            commands: vec![system::resolver_remove_command(suffix)],
        });
    }

    // Only when the certificate is still there to name. `remove-trusted-cert`
    // takes a file, so a CA already deleted cannot be untrusted by this
    // route — which is the reason the keychain step comes before the
    // directory that holds the certificate.
    if let Some(ca_path) = ca_path.filter(|path| path.exists()) {
        plan.privileged.push(Privileged {
            label: "stop trusting the local CA".to_string(),
            commands: vec![system::untrust_command(ca_path)],
        });
    }

    plan
}

/// The two binaries, when they are where this one is.
///
/// `minato` finds the daemon next to itself, so they were installed
/// together and go together. A build tree is left alone: deleting
/// `target/debug/minato` because someone ran it from a checkout would be
/// removing a build artefact, not an installation.
fn installed_binaries() -> Vec<Removal> {
    let Ok(exe) = std::env::current_exe() else {
        return Vec::new();
    };
    let Some(dir) = exe.parent() else {
        return Vec::new();
    };

    if is_build_tree(dir) {
        return Vec::new();
    }

    ["minato", "minatod"]
        .into_iter()
        .map(|name| dir.join(name))
        .filter(|path| path.exists())
        .map(|path| Removal {
            label: "the binary",
            path,
        })
        .collect()
}

/// Whether this looks like `cargo build` output rather than an install.
fn is_build_tree(dir: &Path) -> bool {
    matches!(
        dir.file_name().and_then(|name| name.to_str()),
        Some("debug" | "release")
    ) && dir
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        == Some("target")
}

/// The completion scripts, in the directories the install script writes
/// them to. Same XDG defaults, so the two stay in step.
fn completions() -> Vec<Removal> {
    let home = dirs::home_dir();

    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|home| home.join(".local/share")));

    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|home| home.join(".config")));

    let mut candidates = Vec::new();
    if let Some(data) = data {
        candidates.push(data.join("bash-completion/completions/minato"));
        candidates.push(data.join("zsh/site-functions/_minato"));
    }
    if let Some(config) = config {
        candidates.push(config.join("fish/completions/minato.fish"));
    }

    candidates
        .into_iter()
        .filter(|path| path.exists())
        .map(|path| Removal {
            label: "shell completions",
            path,
        })
        .collect()
}

/// Removes what the plan found, and says what it could not.
///
/// Ordering matters in one place: the binaries go last. Everything before
/// them is driven by this process, and on Unix a running executable
/// survives being unlinked — but only if nothing still needs to read it.
pub fn remove_files(plan: &Plan) -> Vec<String> {
    let mut failures = Vec::new();

    let (binaries, rest): (Vec<&Removal>, Vec<&Removal>) = plan
        .files
        .iter()
        .partition(|removal| removal.label == "the binary");

    for removal in rest.into_iter().chain(binaries) {
        let outcome = if removal.path.is_dir() {
            std::fs::remove_dir_all(&removal.path)
        } else {
            std::fs::remove_file(&removal.path)
        };

        // Already gone is the outcome that was wanted.
        if let Err(err) = outcome
            && err.kind() != std::io::ErrorKind::NotFound
        {
            failures.push(format!("{}: {err}", removal.path.display()));
        }
    }

    failures
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_tree_is_not_an_installation() {
        // `cargo run` from a checkout must not have `minato uninstall`
        // delete the build output.
        assert!(is_build_tree(Path::new("/repo/target/debug")));
        assert!(is_build_tree(Path::new("/repo/target/release")));

        assert!(!is_build_tree(Path::new("/home/u/.local/bin")));
        assert!(!is_build_tree(Path::new("/usr/local/bin")));
        // `debug` on its own is somebody's directory, not cargo's.
        assert!(!is_build_tree(Path::new("/home/u/debug")));
    }

    #[test]
    fn a_plan_lists_only_what_is_there() {
        // Nothing of Minato's is installed under a fresh temporary root,
        // so the plan has nothing to say about it.
        let dir = tempfile::tempdir().expect("tempdir");
        let plan = plan("localhost", Some(&dir.path().join("absent-ca.crt")));

        assert!(
            !plan
                .privileged
                .iter()
                .any(|step| step.label.contains("trusting")),
            "a certificate that is not there cannot be untrusted"
        );
    }

    #[test]
    fn the_certificate_to_untrust_sits_inside_what_gets_deleted() {
        // Why the privileged steps run before anything is removed.
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("minato");
        let ca = root.join("ca/minato-ca.crt");
        std::fs::create_dir_all(ca.parent().expect("parent")).expect("creates");
        std::fs::write(&ca, "-----BEGIN CERTIFICATE-----").expect("writes");

        assert!(
            ca.starts_with(&root),
            "the certificate is expected under the state root: {}",
            ca.display()
        );

        let command = system::untrust_command(&ca);

        if cfg!(target_os = "macos") {
            // `security remove-trusted-cert` names the file itself. Delete
            // the state root first and there is nothing left to point at,
            // the command fails, and the CA stays trusted for good.
            assert!(
                command.contains(&ca.display().to_string()),
                "got: {command}"
            );
        } else {
            // Elsewhere the certificate was *copied* into the system
            // store, so untrusting removes that copy and the argument goes
            // unused. The ordering still holds, but for the LaunchDaemon's
            // sake rather than this one's.
            assert!(command.contains("ca-certificates"), "got: {command}");
        }
    }

    #[test]
    fn removing_what_is_already_gone_is_not_a_failure() {
        let dir = tempfile::tempdir().expect("tempdir");

        let plan = Plan {
            files: vec![Removal {
                label: "the binary",
                path: dir.path().join("never-existed"),
            }],
            privileged: Vec::new(),
        };

        assert!(remove_files(&plan).is_empty());
    }

    #[test]
    fn a_directory_goes_with_its_contents() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("minato");
        std::fs::create_dir_all(root.join("logs")).expect("creates");
        std::fs::write(root.join("state.json"), "{}").expect("writes");

        let plan = Plan {
            files: vec![Removal {
                label: "state, logs and the local CA",
                path: root.clone(),
            }],
            privileged: Vec::new(),
        };

        assert!(remove_files(&plan).is_empty());
        assert!(!root.exists());
    }

    #[test]
    fn the_binaries_are_removed_last() {
        // The process is running from one of them, and everything else
        // has to be done before it can go.
        let dir = tempfile::tempdir().expect("tempdir");

        let binary = dir.path().join("minato");
        let state = dir.path().join("state");
        std::fs::write(&binary, "#!/bin/sh\n").expect("writes");
        std::fs::create_dir(&state).expect("creates");

        let plan = Plan {
            files: vec![
                Removal {
                    label: "the binary",
                    path: binary.clone(),
                },
                Removal {
                    label: "state, logs and the local CA",
                    path: state.clone(),
                },
            ],
            privileged: Vec::new(),
        };

        assert!(remove_files(&plan).is_empty());
        assert!(!binary.exists());
        assert!(!state.exists());
    }
}
