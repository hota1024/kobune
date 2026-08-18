//! Mapping the [`Target`] a client sends onto a real project and
//! workspace.
//!
//! Neither the CLI nor the GUI knows more than the directory it is in.
//! Getting from there to a git repository, a configuration and a workspace
//! is this module's job.

use std::path::Path;

use chrono::Utc;
use kobune_api::Target;
use kobune_core::git::{Repository, Worktree};
use kobune_core::{Error, KobuneConfig, Result, State, WorkspaceRecord};

/// What a [`Target`] resolved to.
pub struct Resolved {
    pub repo: Repository,
    pub config: KobuneConfig,
    pub project: String,
    /// The workspace being acted on.
    pub workspace: WorkspaceRecord,
}

/// Finds the project from `cwd` and reads its configuration.
///
/// Finding the workspace is [`resolve_workspace`]'s job. Creating
/// operations act on a workspace that does not exist yet, hence the two
/// steps.
///
/// `home` is `$KOBUNE_HOME`, which holds the machine-wide layer. The three
/// layers and how they are anchored are [`KobuneConfig::resolve`]'s
/// business; what matters here is that the main worktree is passed along,
/// because that is where the gitignored layer lives.
pub fn resolve_project(target: &Target, state: &mut State, home: &Path) -> Result<ProjectContext> {
    let repo = Repository::discover(&target.cwd)?;

    let config = KobuneConfig::resolve(&repo.root, &repo.main_root, home)?;

    let project = config.project.name.clone();
    state.upsert_project(&project, &repo.main_root)?;

    Ok(ProjectContext {
        repo,
        config,
        project,
    })
}

pub struct ProjectContext {
    pub repo: Repository,
    pub config: KobuneConfig,
    pub project: String,
}

impl ProjectContext {
    /// Decides which workspace to act on.
    ///
    /// 1. Use `target.workspace`'s label when one was given
    /// 2. Otherwise use the worktree `cwd` sits in
    ///
    /// A worktree created by `git worktree add` outside Kobune gets
    /// registered here too, so nobody is left feeling they created their
    /// worktree the wrong way.
    pub fn resolve_workspace(self, target: &Target, state: &mut State) -> Result<Resolved> {
        let worktrees = self.repo.worktrees()?;

        let workspace = match &target.workspace {
            Some(label) => self.lookup_by_label(label, &worktrees, state)?,
            None => self.lookup_by_path(&self.repo.root, &worktrees, state)?,
        };

        Ok(Resolved {
            repo: self.repo,
            config: self.config,
            project: self.project,
            workspace,
        })
    }

    fn lookup_by_label(
        &self,
        label: &str,
        worktrees: &[Worktree],
        state: &mut State,
    ) -> Result<WorkspaceRecord> {
        let record = state
            .project(&self.project)
            .and_then(|p| p.workspace(label))
            .cloned();

        if let Some(record) = record {
            // Catch a registration whose worktree has since gone.
            if !worktrees.iter().any(|wt| wt.path == record.path) {
                return Err(Error::WorkspaceNotFound(format!(
                    "{label} (the registered worktree {} is gone)",
                    record.path.display()
                )));
            }
            return Ok(record);
        }

        Err(Error::WorkspaceNotFound(label.to_string()))
    }

    fn lookup_by_path(
        &self,
        path: &Path,
        worktrees: &[Worktree],
        state: &mut State,
    ) -> Result<WorkspaceRecord> {
        if let Some(record) = state
            .project(&self.project)
            .and_then(|p| p.workspace_by_path(path))
            .cloned()
        {
            return Ok(record);
        }

        let worktree = worktrees
            .iter()
            .find(|wt| wt.path == path)
            .ok_or_else(|| Error::WorkspaceNotFound(path.display().to_string()))?;

        self.register(worktree, state)
    }

    /// Registers a worktree Kobune does not know about yet.
    pub fn register(&self, worktree: &Worktree, state: &mut State) -> Result<WorkspaceRecord> {
        let branch = worktree
            .branch
            .clone()
            .unwrap_or_else(|| detached_name(worktree));

        let project = state
            .project_mut(&self.project)
            .ok_or_else(|| Error::WorkspaceNotFound(self.project.clone()))?;

        let record = WorkspaceRecord {
            label: project.allocate_label(&branch),
            branch,
            path: worktree.path.clone(),
            is_main: worktree.path == self.repo.main_root,
            created_at: Utc::now(),
            setup_done: Default::default(),
        };

        project.insert_workspace(record.clone());
        Ok(record)
    }

    /// The registration for this worktree, created if there is none.
    pub fn ensure_registered(
        &self,
        worktree: &Worktree,
        state: &mut State,
    ) -> Result<WorkspaceRecord> {
        if let Some(record) = state
            .project(&self.project)
            .and_then(|p| p.workspace_by_path(&worktree.path))
            .cloned()
        {
            return Ok(record);
        }

        self.register(worktree, state)
    }
}

/// What to call a worktree on a detached HEAD.
fn detached_name(worktree: &Worktree) -> String {
    let head = worktree
        .head
        .as_deref()
        .map(|h| &h[..h.len().min(7)])
        .unwrap_or("unknown");

    format!("detached-{head}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn worktree(path: &str, branch: Option<&str>, head: Option<&str>) -> Worktree {
        Worktree {
            path: PathBuf::from(path),
            head: head.map(str::to_string),
            branch: branch.map(str::to_string),
            detached: branch.is_none(),
            bare: false,
            locked: false,
        }
    }

    #[test]
    fn names_detached_worktrees_by_head() {
        let wt = worktree("/repo/wt/x", None, Some("abc123def456"));
        assert_eq!(detached_name(&wt), "detached-abc123d");
    }

    #[test]
    fn tolerates_missing_head() {
        let wt = worktree("/repo/wt/x", None, None);
        assert_eq!(detached_name(&wt), "detached-unknown");
    }

    #[test]
    fn tolerates_short_head() {
        let wt = worktree("/repo/wt/x", None, Some("abc"));
        assert_eq!(detached_name(&wt), "detached-abc");
    }
}
