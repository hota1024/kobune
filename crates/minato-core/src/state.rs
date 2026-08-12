//! The workspace registry.
//!
//! **No runtime state lives here.** Whether a container is alive and which
//! port it got are read from the runtime's own labels (`dev.minato.*`), so
//! the daemon can recover them after a restart. This store only records
//! which worktrees Minato manages and the URL label issued to each.
//!
//! Labels are persisted so that changing the rules in [`crate::naming`]
//! later does not change the URL of an existing workspace.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::ServiceScope;
use crate::error::{Error, Result};
use crate::naming;

/// The schema version of the state file.
pub const CURRENT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub version: u32,

    /// Keyed by project name (`[project] name` in `minato.toml`).
    ///
    /// Since the name appears in URLs, two projects with the same name
    /// cannot coexist. So the name doubles as the identifier and clashes
    /// are rejected at registration time.
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectRecord>,

    /// The Cloudflare Tunnel, if one has been set up.
    ///
    /// Machine-wide rather than per-project: one named tunnel carries every
    /// project, and the project name is a label in the hostname
    /// (`docs/DESIGN.md` §9).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tunnel: Option<TunnelRecord>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            projects: BTreeMap::new(),
            tunnel: None,
        }
    }
}

/// A configured Cloudflare Tunnel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunnelRecord {
    /// The named tunnel that carries the traffic.
    pub name: String,
    /// The zone the hostnames live under (`example.com`).
    pub domain: String,
    /// Whether the daemon should be running it.
    ///
    /// `disable` clears this but keeps the record, so re-enabling does not
    /// mean naming the domain again.
    #[serde(default)]
    pub enabled: bool,
    /// Projects a DNS route has been created for.
    ///
    /// One wildcard record per project (`*.{project}.{domain}`), so
    /// workspaces come and go without touching DNS.
    #[serde(default)]
    pub routed: BTreeSet<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRecord {
    pub name: String,
    /// The path of the main worktree.
    pub root: PathBuf,
    /// Keyed by the workspace label used in URLs.
    #[serde(default)]
    pub workspaces: BTreeMap<String, WorkspaceRecord>,
    /// The `setup` already run for services shared by the whole project.
    ///
    /// Separate from the per-worktree map because that is where the
    /// container lives: one instance for every worktree.
    #[serde(default)]
    pub setup_done: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceRecord {
    /// The name that appears in URLs. Never changed once issued.
    pub label: String,
    /// The original branch name, before sanitisation.
    pub branch: String,
    pub path: PathBuf,
    /// Whether this is the main worktree. Its label is omitted from URLs.
    #[serde(default)]
    pub is_main: bool,
    pub created_at: DateTime<Utc>,
    /// The `setup` that has already run, per service.
    ///
    /// The value is the command itself, so editing it is what makes it run
    /// again — there is nothing else to compare, and a version number
    /// would be one more thing to remember to change.
    #[serde(default)]
    pub setup_done: BTreeMap<String, String>,
}

impl State {
    /// Whether `setup` still has to run for this service.
    ///
    /// **Remembered where the container lives.** A `scope = "project"`
    /// service has one instance for every worktree, so keeping the record
    /// per worktree would run its setup once per worktree against the
    /// single thing it set up.
    pub fn needs_setup(
        &self,
        project: &str,
        workspace: &str,
        service: &str,
        scope: ServiceScope,
        setup: &str,
    ) -> bool {
        let Some(record) = self.projects.get(project) else {
            return true;
        };

        let done = match scope {
            ServiceScope::Project => record.setup_done.get(service),
            ServiceScope::Workspace => record
                .workspaces
                .get(workspace)
                .and_then(|workspace| workspace.setup_done.get(service)),
        };

        done.map(String::as_str) != Some(setup)
    }

    /// Remembers that `setup` has run. `false` if there was nowhere to put
    /// it, which means the workspace went while it was running.
    pub fn record_setup(
        &mut self,
        project: &str,
        workspace: &str,
        service: &str,
        scope: ServiceScope,
        setup: &str,
    ) -> bool {
        let Some(record) = self.projects.get_mut(project) else {
            return false;
        };

        let done = match scope {
            ServiceScope::Project => &mut record.setup_done,
            ServiceScope::Workspace => match record.workspaces.get_mut(workspace) {
                Some(workspace) => &mut workspace.setup_done,
                None => return false,
            },
        };

        done.insert(service.to_string(), setup.to_string());
        true
    }
}

impl State {
    /// Registers a project. Fails if the same name is already registered
    /// at a different path.
    pub fn upsert_project(&mut self, name: &str, root: &Path) -> Result<&mut ProjectRecord> {
        match self.projects.get(name) {
            Some(existing) if existing.root != root => {
                return Err(Error::ConfigInvalid(format!(
                    "the project name `{name}` is already registered at {}. \
                     Their URLs would collide, so change one of the [project] names",
                    existing.root.display()
                )));
            }
            _ => {}
        }

        Ok(self
            .projects
            .entry(name.to_string())
            .or_insert_with(|| ProjectRecord {
                setup_done: BTreeMap::new(),
                name: name.to_string(),
                root: root.to_path_buf(),
                workspaces: BTreeMap::new(),
            }))
    }

    pub fn project(&self, name: &str) -> Option<&ProjectRecord> {
        self.projects.get(name)
    }

    pub fn project_mut(&mut self, name: &str) -> Option<&mut ProjectRecord> {
        self.projects.get_mut(name)
    }
}

impl WorkspaceRecord {
    /// The workspace label to embed in URLs.
    ///
    /// Omitted for the main worktree, giving `{service}.{project}.localhost`.
    pub fn url_label(&self) -> Option<&str> {
        if self.is_main {
            None
        } else {
            Some(&self.label)
        }
    }
}

impl ProjectRecord {
    /// Looks up a workspace by path.
    pub fn workspace_by_path(&self, path: &Path) -> Option<&WorkspaceRecord> {
        self.workspaces.values().find(|ws| ws.path == path)
    }

    pub fn workspace(&self, label: &str) -> Option<&WorkspaceRecord> {
        self.workspaces.get(label)
    }

    /// Picks an unused workspace label for a branch.
    ///
    /// Returns the existing label if the branch is already registered, so
    /// repeated calls are idempotent.
    pub fn allocate_label(&self, branch: &str) -> String {
        if let Some(existing) = self.workspaces.values().find(|ws| ws.branch == branch) {
            return existing.label.clone();
        }

        let base = naming::sanitize_label(branch);
        if !self.workspaces.contains_key(&base) {
            return base;
        }

        // Two branches can sanitise to the same shape. The suffix is
        // derived from the branch name, so the same clash always yields
        // the same label.
        let disambiguated = naming::disambiguate(&base, branch);
        if !self.workspaces.contains_key(&disambiguated) {
            return disambiguated;
        }

        // Only reached when the hashes collide too. Escape with a counter.
        for n in 2..1000 {
            let candidate = naming::disambiguate(&base, &format!("{branch}#{n}"));
            if !self.workspaces.contains_key(&candidate) {
                return candidate;
            }
        }

        unreachable!("1000 attempts without a free label cannot happen in practice");
    }

    pub fn insert_workspace(&mut self, record: WorkspaceRecord) {
        self.workspaces.insert(record.label.clone(), record);
    }

    pub fn remove_workspace(&mut self, label: &str) -> Option<WorkspaceRecord> {
        self.workspaces.remove(label)
    }
}

/// Reads and writes the state file.
///
/// Does no cross-process locking. The daemon is the single writer, and
/// serialises its own access with a `Mutex`.
///
/// **What makes it the single writer is the socket**, not a lock here: a
/// second daemon tries to connect to `minatod.sock` before anything else
/// and stands down when something answers (`apps/daemon/src/server.rs`).
/// So there is exactly one process in a position to write this file.
#[derive(Debug, Clone)]
pub struct StateStore {
    path: PathBuf,
}

impl StateStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads the state. Returns an empty state when the file is absent.
    pub fn load(&self) -> Result<State> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(State::default());
            }
            Err(source) => {
                return Err(Error::StateIo {
                    path: self.path.clone(),
                    source,
                });
            }
        };

        let state: State = serde_json::from_str(&text).map_err(|source| Error::StateCorrupt {
            path: self.path.clone(),
            source,
        })?;

        if state.version > CURRENT_VERSION {
            return Err(Error::ConfigInvalid(format!(
                "the state file {} is at version {}, which this minato \
                 (supporting version {}) cannot read. Update minato",
                self.path.display(),
                state.version,
                CURRENT_VERSION
            )));
        }

        Ok(state)
    }

    /// Writes the state out. Writes to a temporary file in the same
    /// directory and renames, so a crash mid-write cannot corrupt it.
    pub fn save(&self, state: &State) -> Result<()> {
        let dir = self.path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(dir).map_err(|source| Error::StateIo {
            path: dir.to_path_buf(),
            source,
        })?;

        let json = serde_json::to_vec_pretty(state).expect("State always serialises");

        let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(|source| Error::StateIo {
            path: dir.to_path_buf(),
            source,
        })?;
        tmp.write_all(&json).map_err(|source| Error::StateIo {
            path: self.path.clone(),
            source,
        })?;
        tmp.as_file().sync_all().map_err(|source| Error::StateIo {
            path: self.path.clone(),
            source,
        })?;
        tmp.persist(&self.path).map_err(|err| Error::StateIo {
            path: self.path.clone(),
            source: err.error,
        })?;

        Ok(())
    }

    /// Loads, mutates and writes back in one go.
    /// Nothing is written if the closure returns an error.
    pub fn update<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut State) -> Result<T>,
    {
        let mut state = self.load()?;
        let value = f(&mut state)?;
        self.save(&state)?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(label: &str, branch: &str) -> WorkspaceRecord {
        WorkspaceRecord {
            label: label.to_string(),
            branch: branch.to_string(),
            path: PathBuf::from(format!("/repo/wt/{label}")),
            is_main: false,
            created_at: Utc::now(),
            setup_done: BTreeMap::new(),
        }
    }

    #[test]
    fn setup_runs_until_it_has_run_with_this_command() {
        let mut state = State::default();
        state
            .upsert_project("myapp", Path::new("/repo"))
            .expect("registers");
        state
            .project_mut("myapp")
            .expect("registered")
            .insert_workspace(record("feat-1", "feature/one"));

        let workspace = ServiceScope::Workspace;
        assert!(state.needs_setup("myapp", "feat-1", "web", workspace, "pnpm install"));

        assert!(state.record_setup("myapp", "feat-1", "web", workspace, "pnpm install"));
        assert!(!state.needs_setup("myapp", "feat-1", "web", workspace, "pnpm install"));

        // Editing it is what makes it run again; there is nothing else to
        // compare against.
        assert!(state.needs_setup("myapp", "feat-1", "web", workspace, "pnpm install --prod"));

        // And it is per service.
        assert!(state.needs_setup("myapp", "feat-1", "api", workspace, "pnpm install"));
    }

    #[test]
    fn a_shared_service_is_set_up_once_for_the_project() {
        // One container serves every worktree, so remembering per worktree
        // would run its setup again from the next worktree — against the
        // single thing it had already set up.
        let mut state = State::default();
        state
            .upsert_project("myapp", Path::new("/repo"))
            .expect("registers");

        let project = state.project_mut("myapp").expect("registered");
        project.insert_workspace(record("feat-1", "feature/one"));
        project.insert_workspace(record("feat-2", "feature/two"));

        let shared = ServiceScope::Project;
        assert!(state.record_setup("myapp", "feat-1", "db", shared, "psql -f schema.sql"));

        assert!(
            !state.needs_setup("myapp", "feat-2", "db", shared, "psql -f schema.sql"),
            "another worktree must not set the same container up again"
        );
    }

    #[test]
    fn a_workspace_setup_is_not_shared_between_worktrees() {
        let mut state = State::default();
        state
            .upsert_project("myapp", Path::new("/repo"))
            .expect("registers");

        let project = state.project_mut("myapp").expect("registered");
        project.insert_workspace(record("feat-1", "feature/one"));
        project.insert_workspace(record("feat-2", "feature/two"));

        let workspace = ServiceScope::Workspace;
        assert!(state.record_setup("myapp", "feat-1", "web", workspace, "pnpm install"));

        assert!(
            state.needs_setup("myapp", "feat-2", "web", workspace, "pnpm install"),
            "each worktree has its own node_modules to fill"
        );
    }

    #[test]
    fn recording_against_a_workspace_that_went_says_so() {
        // `minato rm` can land while a setup is running.
        let mut state = State::default();
        state
            .upsert_project("myapp", Path::new("/repo"))
            .expect("registers");

        assert!(!state.record_setup(
            "myapp",
            "gone",
            "web",
            ServiceScope::Workspace,
            "pnpm install"
        ));
    }

    fn project() -> ProjectRecord {
        ProjectRecord {
            setup_done: BTreeMap::new(),
            name: "myapp".into(),
            root: PathBuf::from("/repo"),
            workspaces: BTreeMap::new(),
        }
    }

    #[test]
    fn allocates_sanitized_label() {
        let p = project();
        assert_eq!(p.allocate_label("feature/user-auth"), "feature-user-auth");
    }

    #[test]
    fn allocation_is_idempotent_for_known_branch() {
        let mut p = project();
        p.insert_workspace(record("feature-user-auth", "feature/user-auth"));
        assert_eq!(p.allocate_label("feature/user-auth"), "feature-user-auth");
    }

    #[test]
    fn disambiguates_colliding_labels() {
        let mut p = project();
        // `feature/x` and `feature_x` sanitise to the same shape.
        p.insert_workspace(record("feature-x", "feature/x"));

        let label = p.allocate_label("feature_x");
        assert_ne!(label, "feature-x");
        assert!(naming::is_valid_label(&label), "got: {label}");

        // Deterministic.
        assert_eq!(label, p.allocate_label("feature_x"));
    }

    #[test]
    fn rejects_same_project_name_at_different_root() {
        let mut state = State::default();
        state
            .upsert_project("myapp", Path::new("/repo/a"))
            .expect("the first registration succeeds");

        let err = state
            .upsert_project("myapp", Path::new("/repo/b"))
            .unwrap_err();
        assert!(err.to_string().contains("already registered"), "got: {err}");
    }

    #[test]
    fn upsert_is_idempotent_for_same_root() {
        let mut state = State::default();
        state
            .upsert_project("myapp", Path::new("/repo"))
            .expect("ok");
        state
            .upsert_project("myapp", Path::new("/repo"))
            .expect("the same path is accepted");
        assert_eq!(state.projects.len(), 1);
    }

    #[test]
    fn load_returns_default_when_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = StateStore::new(dir.path().join("state.json"));

        let state = store.load().expect("loads");
        assert_eq!(state.version, CURRENT_VERSION);
        assert!(state.projects.is_empty());
    }

    #[test]
    fn roundtrips_through_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = StateStore::new(dir.path().join("state.json"));

        store
            .update(|state| {
                let project = state.upsert_project("myapp", Path::new("/repo"))?;
                project.insert_workspace(record("feat-1", "feature/one"));
                Ok(())
            })
            .expect("writes");

        let loaded = store.load().expect("loads");
        let project = loaded.project("myapp").expect("exists");
        assert_eq!(project.root, PathBuf::from("/repo"));

        let ws = project.workspace("feat-1").expect("exists");
        assert_eq!(ws.branch, "feature/one");
    }

    #[test]
    fn update_does_not_write_on_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = StateStore::new(dir.path().join("state.json"));

        let result: Result<()> = store.update(|state| {
            state.upsert_project("myapp", Path::new("/repo"))?;
            Err(Error::ConfigInvalid("failed midway".into()))
        });
        assert!(result.is_err());

        assert!(
            store.load().expect("loads").projects.is_empty(),
            "a failed update is not persisted"
        );
    }

    #[test]
    fn rejects_future_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");
        std::fs::write(&path, r#"{"version": 999, "projects": {}}"#).expect("writes");

        let err = StateStore::new(path).load().unwrap_err();
        assert!(err.to_string().contains("Update minato"), "got: {err}");
    }

    #[test]
    fn reports_corrupt_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");
        std::fs::write(&path, "not json").expect("writes");

        let err = StateStore::new(path).load().unwrap_err();
        assert!(err.to_string().contains("corrupt"), "got: {err}");
    }
}
