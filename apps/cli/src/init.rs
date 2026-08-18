//! `kobune init` — writes a starter `kobune.toml`.
//!
//! It never prompts; that would put it out of an agent's reach. Whatever
//! can be inferred is inferred, and the rest is a commented template.

use std::path::{Path, PathBuf};

use kobune_core::config::{CONFIG_FILE, LOCAL_CONFIG_FILE};
use kobune_core::{Repository, env, git, naming};

#[derive(Debug)]
pub struct InitOutcome {
    pub path: PathBuf,
    pub project: String,
    /// The compose file it was converted from, when it was.
    pub from: Option<PathBuf>,
    /// Keys compose had that Kobune has no answer for, per service.
    ///
    /// **Never empty because nothing was lost — empty because nothing
    /// was.** A conversion that quietly leaves things out produces a
    /// file that looks finished and is not.
    pub dropped: Vec<(String, String)>,
    /// Files compose read environments from, now `carry`.
    pub carried: Vec<String>,
    /// What became of `.gitignore`.
    pub ignore: Ignore,
}

/// What `init` did about the files that must not be committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ignore {
    /// Not a git repository, so there is nothing to ignore anything.
    NoRepository,
    /// Git's rules already cover every entry, by whatever route.
    AlreadyCovered,
    /// These were written to `path`, which was created if `created`.
    Added {
        path: PathBuf,
        entries: Vec<String>,
        created: bool,
    },
    /// The file could not be written.
    ///
    /// **Reported, never fatal.** The configuration file is what was
    /// asked for and it is already on disk; failing the command over the
    /// convenience beside it would be the wrong way round.
    Failed(String),
}

/// The files Kobune writes or reads that belong to one machine.
///
/// **`.kobune/env` is deliberately absent.** The project's environment
/// layer is meant to be committed, so ignoring the directory would take it
/// along with the two files that should not be.
///
/// Joined with `/` rather than through `Path`: a gitignore pattern is
/// matched against a path git spells with forward slashes, on every
/// platform.
fn never_commit() -> [String; 2] {
    [
        LOCAL_CONFIG_FILE.to_string(),
        format!("{}/{}", env::ENV_DIR, env::WORKSPACE_ENV_FILE),
    ]
}

/// The line that says why the block underneath is there.
const IGNORE_HEADING: &str = "# kobune: yours, not the project's";

/// Adds ignore rules for what must not be committed.
///
/// Only what git does not already cover, so running `init --force` twice
/// does not append the block twice, and a repository that ignores
/// `*.local.toml` already is left alone.
fn ensure_ignored(root: &Path) -> Ignore {
    let path = root.join(".gitignore");
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Ignore::Failed(err.to_string()),
    };

    let written = existing.as_deref().unwrap_or_default();

    let missing: Vec<String> = never_commit()
        .into_iter()
        .filter(|entry| !git::is_ignored(root, entry) && !is_unignored(written, entry))
        .collect();

    if missing.is_empty() {
        return Ignore::AlreadyCovered;
    }

    let created = existing.is_none();
    let mut out = existing.unwrap_or_default();

    // A file somebody wrote by hand is being appended to, so leave its
    // last line ending the way it did and put a blank line between their
    // block and this one.
    if !out.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }

    out.push_str(IGNORE_HEADING);
    out.push('\n');
    for entry in &missing {
        out.push_str(entry);
        out.push('\n');
    }

    match std::fs::write(&path, out) {
        Ok(()) => Ignore::Added {
            path,
            entries: missing,
            created,
        },
        Err(err) => Ignore::Failed(err.to_string()),
    }
}

/// Whether `.gitignore` deliberately un-ignores `entry`.
///
/// **"Is it ignored" is not "is there no rule about it".** A repository
/// ending in `!kobune.local.toml` has somebody's decision in it — a shared
/// override they chose to commit, or one they are debugging — and
/// `check-ignore` reports the path as not ignored, which is exactly what
/// it should say. Appending the plain entry below that line would reverse
/// the decision silently, because the last matching pattern wins.
///
/// Only the exact name, and only in this file. A negation by pattern is
/// somebody being clever, and guessing at what a pattern meant is worse
/// than leaving their file alone.
fn is_unignored(gitignore: &str, entry: &str) -> bool {
    gitignore
        .lines()
        .map(str::trim)
        .any(|line| line.strip_prefix('!').is_some_and(|rest| rest == entry))
}

/// Where the configuration goes, and whether git is there at all.
///
/// Outside a repository the current directory still gets a `kobune.toml` —
/// running `kobune init` in a fresh directory is a reasonable thing to do —
/// but there is nothing for a `.gitignore` to be for.
fn destination(cwd: &Path) -> (PathBuf, bool) {
    match Repository::discover(cwd) {
        Ok(repo) => (repo.main_root, true),
        Err(_) => (cwd.to_path_buf(), false),
    }
}

/// Converts a compose file rather than writing the template.
///
/// `explicit` names one; without it the usual names are tried in the
/// order compose itself tries them.
pub fn from_compose(
    cwd: &Path,
    explicit: Option<&Path>,
    force: bool,
) -> anyhow::Result<InitOutcome> {
    let (root, in_repository) = destination(cwd);

    let path = match explicit {
        Some(named) => {
            let named = if named.is_absolute() {
                named.to_path_buf()
            } else {
                root.join(named)
            };

            if !named.is_file() {
                anyhow::bail!("{} does not exist", named.display());
            }
            named
        }
        None => crate::compose::CANDIDATES
            .iter()
            .map(|name| root.join(name))
            .find(|candidate| candidate.is_file())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "no compose file in {}: looked for {}. Name one with \
                     --from-compose <FILE>",
                    root.display(),
                    crate::compose::CANDIDATES.join(", ")
                )
            })?,
    };

    let destination = root.join(CONFIG_FILE);
    if destination.exists() && !force {
        anyhow::bail!(
            "{} already exists. Pass --force to overwrite it",
            destination.display()
        );
    }

    let yaml = std::fs::read_to_string(&path)?;
    let project = project_name_from(&root);
    let converted = crate::compose::convert(&project, &path.display().to_string(), &yaml)?;

    std::fs::write(&destination, &converted.toml)?;

    Ok(InitOutcome {
        path: destination,
        project,
        from: Some(path),
        dropped: converted
            .dropped
            .into_iter()
            .map(|entry| (entry.service, entry.key))
            .collect(),
        carried: converted.carried,
        ignore: ignore_for(&root, in_repository),
    })
}

/// The ignore step, where there is a repository for it to mean anything.
fn ignore_for(root: &Path, in_repository: bool) -> Ignore {
    match in_repository {
        true => ensure_ignored(root),
        false => Ignore::NoRepository,
    }
}

pub fn run(cwd: &Path, force: bool) -> anyhow::Result<InitOutcome> {
    // It goes at the repository root. Run from inside a worktree, it
    // still lands in the main one.
    let (root, in_repository) = destination(cwd);

    let path = root.join(CONFIG_FILE);
    if path.exists() && !force {
        anyhow::bail!(
            "{} already exists. Pass --force to overwrite it",
            path.display()
        );
    }

    let project = project_name_from(&root);
    std::fs::write(&path, template(&project))?;

    Ok(InitOutcome {
        path,
        project,
        from: None,
        dropped: Vec::new(),
        carried: Vec::new(),
        ignore: ignore_for(&root, in_repository),
    })
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
        r#"# Every key: https://kobune.1024.works/reference/kobune-toml

[project]
name = "{project}"

# The URL suffix. Defaults to {project}.localhost.
# domain = "{project}.localhost"

# Files `kobune new` copies into a new worktree. `git worktree add` brings
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
command = "sh -c 'echo kobune ready; sleep infinity'"

# Run once before the service first starts, not on every up. Put installs
# here so `command` is left doing nothing but starting the app.
# setup = "sh -c 'pnpm install --frozen-lockfile'"

# How readiness is decided. Waited for on start, and again when
# scale-to-zero wakes it. A TCP connect when left out.
# health = "http://localhost:3000/healthz"

# How long without a request before it stops itself.
# idle_timeout = "30m"

# Every service is told the others' URLs as KOBUNE_URL_<SERVICE>, and the
# hostname on its own — no scheme, no port — as KOBUNE_HOSTNAME_<SERVICE>,
# which is what a CORS origin or a cookie domain wants. `${{...}}` puts one
# under the name the app already reads; a bare $NAME does not.
# env = {{ NEXT_PUBLIC_API_URL = "${{KOBUNE_URL_API}}" }}

# For a tool that reads a file rather than its own environment — wrangler
# dev, Vite, dotenvx — the same values, written before the service starts.
# Secrets are left out of it, and a path git tracks is refused.
# env_file = ".kobune/env.app"

# A second service. `depends_on` starts db first and waits for it to be
# ready. Each one gets its own URL — KOBUNE_URL_DB here.
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
# KOBUNE_CACHE_DIR=/var/cache/kobune, a volume shared by the project's
# worktrees. Point package managers at it so they stop writing into the
# repository. `${{...}}` refers to another variable; a bare $NAME does not.
# env = {{ npm_config_store_dir = "${{KOBUNE_CACHE_DIR}}/pnpm" }}
# env = {{ POSTGRES_PASSWORD = "postgres" }}
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use kobune_core::KobuneConfig;

    #[test]
    fn generated_template_is_valid() {
        // A template that does not parse means `up` fails right after
        // `init`.
        let text = template("myapp");
        let config: KobuneConfig = toml::from_str(&text).expect("is syntactically valid");
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

    /// A repository with nothing in it, which is what `init` meets.
    ///
    /// **The developer's own ignore rules are switched off in it.**
    /// `ensure_ignored` asks `git check-ignore`, which consults
    /// `core.excludesFile` — and `*.local.toml` or `*.local.*` is a common
    /// entry in a personal global ignore. Without this the fixture is
    /// whatever machine it runs on, and these tests fail for exactly the
    /// people who keep a tidy one. Repository config outranks both the
    /// global and system files, so `/dev/null` here is the whole of it.
    fn repository() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");

        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .current_dir(dir.path())
                .args(args)
                .status()
                .expect("git runs")
                .success();
            assert!(ok, "git {}", args.join(" "));
        };

        git(&["init", "--quiet"]);
        git(&["config", "core.excludesFile", "/dev/null"]);

        dir
    }

    fn gitignore(root: &Path) -> String {
        std::fs::read_to_string(root.join(".gitignore")).expect("was written")
    }

    #[test]
    fn writes_a_gitignore_for_what_must_not_be_committed() {
        let dir = repository();
        let outcome = run(dir.path(), false).expect("writes it");

        let Ignore::Added {
            entries, created, ..
        } = &outcome.ignore
        else {
            panic!("expected an addition, got {:?}", outcome.ignore);
        };

        assert!(created, "there was no .gitignore to start with");
        assert_eq!(entries, &["kobune.local.toml", ".kobune/env.local"]);

        let text = gitignore(dir.path());
        assert!(text.contains("kobune.local.toml"), "got:\n{text}");
        assert!(text.contains(".kobune/env.local"), "got:\n{text}");
        assert!(
            !text.contains("\n.kobune/env\n"),
            "the project layer is committed, so it must survive:\n{text}"
        );
    }

    #[test]
    fn appends_without_disturbing_what_was_there() {
        let dir = repository();
        std::fs::write(dir.path().join(".gitignore"), "node_modules\n/dist\n").expect("writes");

        run(dir.path(), false).expect("writes it");

        let text = gitignore(dir.path());
        assert!(text.starts_with("node_modules\n/dist\n"), "got:\n{text}");
        assert!(text.contains("kobune.local.toml"), "got:\n{text}");
    }

    #[test]
    fn a_file_with_no_trailing_newline_is_still_appended_to() {
        // Without the fix-up, the entry would land on the end of somebody
        // else's line and ignore neither of them.
        let dir = repository();
        std::fs::write(dir.path().join(".gitignore"), "node_modules").expect("writes");

        run(dir.path(), false).expect("writes it");

        let text = gitignore(dir.path());
        assert!(text.contains("\nnode_modules\n") || text.starts_with("node_modules\n"));
        assert!(
            text.lines().any(|line| line == "kobune.local.toml"),
            "the entry is a line of its own:\n{text}"
        );
    }

    #[test]
    fn running_it_twice_does_not_append_twice() {
        // `init --force` is a thing people run, and a block that grew
        // every time would be found later, in a diff, and wondered at.
        let dir = repository();

        run(dir.path(), false).expect("writes it");
        let second = run(dir.path(), true).expect("--force overwrites");

        assert_eq!(second.ignore, Ignore::AlreadyCovered);
        assert_eq!(
            gitignore(dir.path()).matches("kobune.local.toml").count(),
            1
        );
    }

    #[test]
    fn a_pattern_that_already_covers_it_is_left_alone() {
        // Asked of git rather than matched by hand, so a rule that covers
        // the name some other way counts.
        let dir = repository();
        std::fs::write(
            dir.path().join(".gitignore"),
            "*.local.toml\n.kobune/env.local\n",
        )
        .expect("writes");

        let outcome = run(dir.path(), false).expect("writes it");

        assert_eq!(outcome.ignore, Ignore::AlreadyCovered);
        assert_eq!(gitignore(dir.path()), "*.local.toml\n.kobune/env.local\n");
    }

    #[test]
    fn only_what_is_missing_is_added() {
        let dir = repository();
        std::fs::write(dir.path().join(".gitignore"), ".kobune/env.local\n").expect("writes");

        let outcome = run(dir.path(), false).expect("writes it");

        let Ignore::Added { entries, .. } = &outcome.ignore else {
            panic!("expected an addition, got {:?}", outcome.ignore);
        };
        assert_eq!(entries, &["kobune.local.toml"]);
        assert_eq!(
            gitignore(dir.path()).matches(".kobune/env.local").count(),
            1
        );
    }

    #[test]
    fn a_deliberate_negation_is_not_reversed() {
        // `!kobune.local.toml` is somebody's decision. Appending the plain
        // entry below it would undo that silently, since the last matching
        // pattern wins — and `git status` would stop showing them a file
        // they meant to track.
        let dir = repository();
        std::fs::write(
            dir.path().join(".gitignore"),
            "node_modules\n!kobune.local.toml\n",
        )
        .expect("writes");

        let outcome = run(dir.path(), false).expect("writes it");

        // The other entry is still added; only the negated one is left be.
        let Ignore::Added { entries, .. } = &outcome.ignore else {
            panic!("expected an addition, got {:?}", outcome.ignore);
        };
        assert_eq!(entries, &[".kobune/env.local"]);

        let text = gitignore(dir.path());
        assert!(
            !text.lines().any(|line| line == "kobune.local.toml"),
            "the negation stands:\n{text}"
        );
    }

    #[test]
    fn outside_a_repository_there_is_nothing_to_ignore() {
        // `kobune init` in a fresh directory still writes the config.
        let dir = tempfile::tempdir().expect("tempdir");
        let outcome = run(dir.path(), false).expect("writes it");

        assert_eq!(outcome.ignore, Ignore::NoRepository);
        assert!(!dir.path().join(".gitignore").exists());
    }
}
