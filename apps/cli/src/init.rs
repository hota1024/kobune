//! `minato init` — writes a starter `minato.toml`.
//!
//! It never prompts; that would put it out of an agent's reach. Whatever
//! can be inferred is inferred, and the rest is a commented template.

use std::path::{Path, PathBuf};

use minato_core::config::CONFIG_FILE;
use minato_core::{Repository, naming};

#[derive(Debug)]
pub struct InitOutcome {
    pub path: PathBuf,
    pub project: String,
}

pub fn run(cwd: &Path, force: bool) -> anyhow::Result<InitOutcome> {
    // It goes at the repository root. Run from inside a worktree, it
    // still lands in the main one.
    let root = match Repository::discover(cwd) {
        Ok(repo) => repo.main_root,
        Err(_) => cwd.to_path_buf(),
    };

    let path = root.join(CONFIG_FILE);
    if path.exists() && !force {
        anyhow::bail!(
            "{} already exists. Pass --force to overwrite it",
            path.display()
        );
    }

    let project = project_name_from(&root);
    std::fs::write(&path, template(&project))?;

    Ok(InitOutcome { path, project })
}

/// Derives the project name from the directory name.
fn project_name_from(root: &Path) -> String {
    let raw = root
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "app".to_string());

    naming::sanitize_label(&raw)
}

fn template(project: &str) -> String {
    format!(
        r#"[project]
name = "{project}"

# The URL suffix. Defaults to {project}.localhost.
# domain = "{project}.localhost"

[runtime]
# Either docker or apple (Apple Container)
default = "docker"

# Define at least one service.
# The worktree source is mounted at /workspace in every container.
[services.app]
image = "node:22"
port = 3000
command = "sh -c 'echo minato ready; sleep infinity'"

# How readiness is decided. Used by scale-to-zero (M2).
# health = "http://localhost:3000/healthz"

# How long without a request before it stops itself.
# idle_timeout = "30m"

# [services.db]
# image = "postgres:16"
# port = 5432
# scope = "project"    # one instance, shared across worktrees
# expose = false       # no URL for this one
# volumes = ["pgdata:/var/lib/postgresql/data"]
# env = {{ POSTGRES_PASSWORD = "postgres" }}
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use minato_core::MinatoConfig;

    #[test]
    fn generated_template_is_valid() {
        // A template that does not parse means `up` fails right after
        // `init`.
        let text = template("myapp");
        let config: MinatoConfig = toml::from_str(&text).expect("is syntactically valid");
        config.validate().expect("is semantically valid");

        assert_eq!(config.project.name, "myapp");
        assert_eq!(config.runtime.default, "docker");
        assert!(config.services.contains_key("app"));
    }

    #[test]
    fn derives_project_name_from_directory() {
        assert_eq!(project_name_from(Path::new("/x/My_App")), "my-app");
        assert_eq!(project_name_from(Path::new("/x/myapp")), "myapp");
    }

    #[test]
    fn writes_config_and_refuses_to_clobber() {
        let dir = tempfile::tempdir().expect("tempdir");

        let outcome = run(dir.path(), false).expect("writes it");
        assert!(outcome.path.is_file());

        let err = run(dir.path(), false).unwrap_err();
        assert!(err.to_string().contains("--force"), "got: {err}");

        run(dir.path(), true).expect("--force overwrites");
    }
}
