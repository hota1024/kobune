//! クライアントが送ってきた [`Target`] を、実際のプロジェクトと workspace に対応づける。
//!
//! CLI も GUI も「今いるディレクトリ」しか教えてくれない。そこから
//! git リポジトリ・設定・workspace を突き止めるのがここの役目。

use std::path::Path;

use chrono::Utc;
use minato_api::Target;
use minato_core::git::{Repository, Worktree};
use minato_core::{Error, MinatoConfig, Result, State, WorkspaceRecord};

/// [`Target`] の解決結果。
pub struct Resolved {
    pub repo: Repository,
    pub config: MinatoConfig,
    pub project: String,
    /// 操作の対象になる workspace。
    pub workspace: WorkspaceRecord,
}

/// `cwd` からプロジェクトを特定し、設定を読む。
///
/// workspace の特定は [`resolve_workspace`] が行う。作成系の操作では
/// 対象 workspace がまだ存在しないため、2 段階に分けている。
pub fn resolve_project(target: &Target, state: &mut State) -> Result<ProjectContext> {
    let repo = Repository::discover(&target.cwd)?;

    // worktree 内にも同じ内容の minato.toml があるため、そこから探せばよい。
    // 見つからなければ main worktree も見る（worktree 作成直後など）。
    let (_config_path, config) = match MinatoConfig::find(&repo.root) {
        Ok(found) => found,
        Err(Error::ConfigNotFound(_)) => MinatoConfig::find(&repo.main_root)?,
        Err(err) => return Err(err),
    };

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
    pub config: MinatoConfig,
    pub project: String,
}

impl ProjectContext {
    /// 対象の workspace を決める。
    ///
    /// 1. `target.workspace` が指定されていればそのラベルを使う
    /// 2. なければ `cwd` が属する worktree を使う
    ///
    /// Minato の外で `git worktree add` された worktree もここで登録する。
    /// 利用者が worktree の作り方を間違えたと感じないようにするため。
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
            // 登録はあるが worktree が消えている場合を検出する。
            if !worktrees.iter().any(|wt| wt.path == record.path) {
                return Err(Error::WorkspaceNotFound(format!(
                    "{label} (登録されている worktree {} が見つかりません)",
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

    /// まだ Minato が知らない worktree を登録する。
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
        };

        project.insert_workspace(record.clone());
        Ok(record)
    }

    /// この worktree に対応する登録を返す。なければ作る。
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

/// detached HEAD の worktree に付ける名前。
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
