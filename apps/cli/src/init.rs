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
        r#"# Every key: https://minato.1024.works/reference/minato-toml

[project]
name = "{project}"

# The URL suffix. Defaults to {project}.localhost.
# domain = "{project}.localhost"

# Files `minato new` copies into a new worktree. `git worktree add` brings
# the tracked files and nothing else, so an untracked but required .env
# would leave the new environment unable to start.
# carry = [".env"]

[runtime]
# Either docker or apple (Apple Container)
default = "docker"

# Define at least one service.
# The worktree source is mounted at /workspace in every container.
# `command` is split shell-style, so quotes group arguments.
[services.app]
image = "node:22"
port = 3000
command = "sh -c 'echo minato ready; sleep infinity'"

# Run once before the service first starts, not on every up. Put installs
# here so `command` is left doing nothing but starting the app.
# setup = "sh -c 'pnpm install --frozen-lockfile'"

# How readiness is decided. Waited for on start, and again when
# scale-to-zero wakes it. A TCP connect when left out.
# health = "http://localhost:3000/healthz"

# How long without a request before it stops itself.
# idle_timeout = "30m"

# Every service is told the others' URLs as MINATO_URL_<SERVICE>, and the
# hostname on its own — no scheme, no port — as MINATO_HOSTNAME_<SERVICE>,
# which is what a CORS origin or a cookie domain wants. `${{...}}` puts one
# under the name the app already reads; a bare $NAME does not.
# env = {{ NEXT_PUBLIC_API_URL = "${{MINATO_URL_API}}" }}

# For a tool that reads a file rather than its own environment — wrangler
# dev, Vite, dotenvx — the same values, written before the service starts.
# Secrets are left out of it, and a path git tracks is refused.
# env_file = ".minato/env.app"

# A second service. `depends_on` starts db first and waits for it to be
# ready. Each one gets its own URL — MINATO_URL_DB here.
# [services.db]
# image = "postgres:16"
# port = 5432
# scope = "project"    # one instance, shared across worktrees
# expose = false       # no URL for this one
# volumes = ["pgdata:/var/lib/postgresql/data"]   # shared across worktrees
#
# `name@workspace:/path` gives each worktree its own volume instead — for
# anything a branch changes the shape of, node_modules being the usual one.
#
# For caches there is nothing to declare: every service is given
# MINATO_CACHE_DIR=/var/cache/minato, a volume shared by the project's
# worktrees. Point package managers at it so they stop writing into the
# repository. `${{...}}` refers to another variable; a bare $NAME does not.
# env = {{ npm_config_store_dir = "${{MINATO_CACHE_DIR}}/pnpm" }}
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
