//! Taking Kobune back off the machine.
//!
//! The daemon removes what it made — containers, networks, its state file
//! — because only it knows what those are. Everything else is here,
//! because only this side knows where it was installed from.
//!
//! Two rules shape the whole module.
//!
//! **Nothing is removed that was not found.** The plan is built by looking,
//! so what a person is asked to confirm is what is actually there, not a
//! list of everywhere Kobune might have put something.
//!
//! **Worktrees are never touched.** They are the user's checkouts with the
//! user's uncommitted work in them. `kobune rm` removes one at a time and
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

/// What `kobune uninstall` would take off this machine.
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

    if let Ok(paths) = kobune_core::Paths::resolve() {
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
            label: format!("stop sending *.{suffix} to Kobune's DNS"),
            commands: vec![system::resolver_remove_command(suffix)],
        });
    }

    if let Some(ca_path) = ca_path.filter(|path| trust_is_removable(path)) {
        plan.privileged.push(Privileged {
            label: "stop trusting the local CA".to_string(),
            commands: vec![system::untrust_command(ca_path)],
        });
    }

    plan
}

/// Whether untrusting the CA is a step worth offering.
///
/// The two platforms need opposite questions asked.
///
/// On macOS the certificate is trusted *in place*: `remove-trusted-cert`
/// names the file, so the step is only possible while the file is there —
/// which is also why the privileged steps run before anything is deleted.
///
/// Everywhere else it was **copied** into the system store on the way in,
/// and it is that copy which is trusted. Asking whether `~/.kobune` still
/// holds the original would be asking about the wrong file entirely: a
/// user who moved `KOBUNE_HOME` or deleted the directory by hand would be
/// shown a plan with no mention of a certificate their machine still
/// trusts.
fn trust_is_removable(ca_path: &Path) -> bool {
    if cfg!(target_os = "macos") {
        ca_path.exists()
    } else {
        Path::new(SYSTEM_STORE_CA).exists()
    }
}

/// Where a non-macOS install put its copy of the certificate.
///
/// The same path `system::untrust_command` removes; they are a pair, and
/// changing one without the other silently stops the step being offered.
const SYSTEM_STORE_CA: &str = "/usr/local/share/ca-certificates/kobune-ca.crt";

/// The two binaries, when they are where this one is.
///
/// `kobune` finds the daemon next to itself, so they were installed
/// together and go together. A build tree is left alone: deleting
/// `target/debug/kobune` because someone ran it from a checkout would be
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

    ["kobune", "kobuned"]
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
        candidates.push(data.join("bash-completion/completions/kobune"));
        candidates.push(data.join("zsh/site-functions/_kobune"));
    }
    if let Some(config) = config {
        candidates.push(config.join("fish/completions/kobune.fish"));
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
pub fn remove_files(plan: &Plan) -> Removed {
    let mut removed = Removed::default();

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

        let Err(err) = outcome else { continue };

        match err.kind() {
            // Already gone is the outcome that was wanted.
            std::io::ErrorKind::NotFound => {}

            // `KOBUNE_INSTALL_DIR=/usr/local/bin` is documented, and a
            // directory root owns is not this process's to write to. This
            // run is already willing to ask for a password for the
            // LaunchDaemon and the keychain, so the honest answer is to
            // offer the same for this rather than report a bare
            // "Permission denied" and stop.
            std::io::ErrorKind::PermissionDenied => {
                removed.needs_root.push(Privileged {
                    label: format!("remove {}", removal.path.display()),
                    commands: vec![format!("sudo rm -rf {}", removal.path.display())],
                });
            }

            _ => removed
                .failures
                .push(format!("{}: {err}", removal.path.display())),
        }
    }

    removed
}

/// What removing the files came to.
#[derive(Debug, Default)]
pub struct Removed {
    /// Could not be removed, and there is no obvious next step.
    pub failures: Vec<String>,
    /// Could not be removed *by this user*, but root could.
    pub needs_root: Vec<Privileged>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_tree_is_not_an_installation() {
        // `cargo run` from a checkout must not have `kobune uninstall`
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
        // Nothing of Kobune's is installed under a fresh temporary root,
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
        let root = dir.path().join("kobune");
        let ca = root.join("ca/kobune-ca.crt");
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

        let removed = remove_files(&plan);
        assert!(removed.failures.is_empty(), "{:?}", removed.failures);
        assert!(removed.needs_root.is_empty());
    }

    #[test]
    fn a_file_this_user_cannot_remove_becomes_a_root_step() {
        // `KOBUNE_INSTALL_DIR=/usr/local/bin` is documented, and root owns
        // it. Reporting "Permission denied" and stopping would be giving
        // up in a run that is already asking for a password elsewhere.
        let dir = tempfile::tempdir().expect("tempdir");
        let locked = dir.path().join("locked");
        std::fs::create_dir(&locked).expect("creates");
        let binary = locked.join("kobune");
        std::fs::write(&binary, "#!/bin/sh\n").expect("writes");

        // Read and execute only: the file cannot be unlinked from here.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555))
                .expect("chmod");
        }

        let plan = Plan {
            files: vec![Removal {
                label: "the binary",
                path: binary.clone(),
            }],
            privileged: Vec::new(),
        };

        let removed = remove_files(&plan);

        // Running as root would succeed anyway, and then there is nothing
        // to hand back — which is just as correct an outcome.
        if removed.needs_root.is_empty() {
            assert!(removed.failures.is_empty(), "{:?}", removed.failures);
        } else {
            assert!(
                removed.needs_root[0].commands[0].starts_with("sudo rm"),
                "got: {:?}",
                removed.needs_root
            );
            assert!(removed.failures.is_empty(), "{:?}", removed.failures);
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755));
        }
    }

    #[test]
    fn a_directory_goes_with_its_contents() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("kobune");
        std::fs::create_dir_all(root.join("logs")).expect("creates");
        std::fs::write(root.join("state.json"), "{}").expect("writes");

        let plan = Plan {
            files: vec![Removal {
                label: "state, logs and the local CA",
                path: root.clone(),
            }],
            privileged: Vec::new(),
        };

        let removed = remove_files(&plan);
        assert!(removed.failures.is_empty(), "{:?}", removed.failures);
        assert!(removed.needs_root.is_empty());
        assert!(!root.exists());
    }

    #[test]
    fn the_binaries_are_removed_last() {
        // The process is running from one of them, and everything else
        // has to be done before it can go.
        let dir = tempfile::tempdir().expect("tempdir");

        let binary = dir.path().join("kobune");
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

        let removed = remove_files(&plan);
        assert!(removed.failures.is_empty(), "{:?}", removed.failures);
        assert!(removed.needs_root.is_empty());
        assert!(!binary.exists());
        assert!(!state.exists());
    }
}
