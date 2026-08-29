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

/// How closely a name fits a workspace, best first.
///
/// **The order is the whole of the rule.** A better kind of fit wins
/// outright — typing a label in full settles it, whatever else that
/// spelling appears inside — and two of the same kind are a question
/// rather than a coin toss. Nothing here scores or weighs: a rule anybody
/// can predict is worth more here than a rule that is right slightly more
/// often, because the thing being chosen is which directory a shell moves
/// to.
///
/// The label comes before the branch at every kind, because the label is
/// what `kobune ls` prints and what `-w` has always taken. A branch is
/// matched at all so that `feature/user-auth` — the name that was typed
/// when the worktree was made, slash and all — still finds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Fit {
    ExactLabel,
    ExactBranch,
    PrefixLabel,
    PrefixBranch,
    SubstringLabel,
    SubstringBranch,
    SubsequenceLabel,
    SubsequenceBranch,
}

/// How well `query` — trimmed and lowercased — fits one workspace.
///
/// The order the tests are written in is [`Fit`]'s own order, and that is
/// not a coincidence to be tidied away: the first one that holds is the
/// answer.
fn fit(query: &str, record: &WorkspaceRecord) -> Option<Fit> {
    let label = record.label.to_lowercase();
    let branch = record.branch.to_lowercase();

    if label == query {
        Some(Fit::ExactLabel)
    } else if branch == query {
        Some(Fit::ExactBranch)
    } else if label.starts_with(query) {
        Some(Fit::PrefixLabel)
    } else if branch.starts_with(query) {
        Some(Fit::PrefixBranch)
    } else if label.contains(query) {
        Some(Fit::SubstringLabel)
    } else if branch.contains(query) {
        Some(Fit::SubstringBranch)
    } else if is_subsequence(&label, query) {
        Some(Fit::SubsequenceLabel)
    } else if is_subsequence(&branch, query) {
        Some(Fit::SubsequenceBranch)
    } else {
        None
    }
}

/// Whether every character of `query` appears in `haystack`, in order.
///
/// What lets `fuauth` reach `feature-user-auth`. The gaps are free and
/// unweighted, which is what keeps [`Fit`] predictable: a tighter match
/// is not a better kind of match here, and the tie it makes with another
/// is answered by asking rather than by scoring.
fn is_subsequence(haystack: &str, query: &str) -> bool {
    let mut chars = haystack.chars();
    query.chars().all(|wanted| chars.any(|c| c == wanted))
}

/// Every workspace a name could mean, with how closely each one fits.
///
/// An empty name is a prefix of everything, so it means all of them —
/// which is what a completion asks for before anything has been typed,
/// and it falls out rather than being a case of its own.
fn ranked<'a>(query: &str, records: &'a [WorkspaceRecord]) -> Vec<(Fit, &'a WorkspaceRecord)> {
    let query = query.trim().to_lowercase();

    let mut ranked: Vec<(Fit, &WorkspaceRecord)> = records
        .iter()
        .filter_map(|record| fit(&query, record).map(|fit| (fit, record)))
        .collect();

    // Alphabetical within a kind, so a listing does not reshuffle itself
    // between one keystroke and the next.
    ranked.sort_by(|(left_fit, left), (right_fit, right)| {
        left_fit
            .cmp(right_fit)
            .then_with(|| left.label.cmp(&right.label))
    });

    ranked
}

/// Every workspace a loosely-typed name could mean, closest first.
pub fn candidates<'a>(query: &str, records: &'a [WorkspaceRecord]) -> Vec<&'a WorkspaceRecord> {
    ranked(query, records)
        .into_iter()
        .map(|(_, record)| record)
        .collect()
}

/// The one workspace a loosely-typed name means.
///
/// **Two equally close matches are an error naming both**, rather than
/// the first of them. Answering "which workspace" with one of several is
/// how a shell ends up in the wrong worktree, and the person who typed
/// the name is the one who knows which they meant.
pub fn find_one<'a>(query: &str, records: &'a [WorkspaceRecord]) -> Result<&'a WorkspaceRecord> {
    let ranked = ranked(query, records);

    let Some((best_fit, best)) = ranked.first().copied() else {
        return Err(Error::WorkspaceNotFound(query.to_string()));
    };

    let tied: Vec<&WorkspaceRecord> = ranked
        .iter()
        .take_while(|(fit, _)| *fit == best_fit)
        .map(|(_, record)| *record)
        .collect();

    if tied.len() > 1 {
        return Err(Error::WorkspaceAmbiguous {
            query: query.trim().to_string(),
            candidates: tied.iter().map(|record| record.label.clone()).collect(),
        });
    }

    Ok(best)
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

    fn record(label: &str, branch: &str) -> WorkspaceRecord {
        WorkspaceRecord {
            label: label.to_string(),
            branch: branch.to_string(),
            path: PathBuf::from(format!("/repo/wt/{label}")),
            is_main: false,
            created_at: Utc::now(),
            setup_done: Default::default(),
        }
    }

    /// `feature/user-auth`, `fix/auth` and `main`, which is enough to put
    /// every kind of fit against every other.
    fn three() -> Vec<WorkspaceRecord> {
        vec![
            record("feature-user-auth", "feature/user-auth"),
            record("fix-auth", "fix/auth"),
            record("main", "main"),
        ]
    }

    fn found(query: &str) -> String {
        find_one(query, &three())
            .map(|record| record.label.clone())
            .unwrap_or_else(|err| panic!("`{query}` did not resolve: {err}"))
    }

    #[test]
    fn a_label_in_full_is_the_answer() {
        assert_eq!(found("feature-user-auth"), "feature-user-auth");
    }

    #[test]
    fn the_branch_that_was_typed_at_kobune_new_still_finds_it() {
        assert_eq!(found("feature/user-auth"), "feature-user-auth");
    }

    #[test]
    fn a_prefix_is_enough() {
        assert_eq!(found("feature"), "feature-user-auth");
    }

    #[test]
    fn characters_in_order_are_enough() {
        assert_eq!(found("fuauth"), "feature-user-auth");
    }

    #[test]
    fn case_does_not_matter() {
        assert_eq!(found("FIX-AUTH"), "fix-auth");
    }

    /// The tie that has to be refused: `auth` sits inside both labels, at
    /// the same kind of fit, and picking either would be a guess.
    #[test]
    fn a_name_that_fits_two_the_same_way_is_a_question() {
        let err = find_one("auth", &three()).expect_err("ambiguous");

        let Error::WorkspaceAmbiguous { candidates, .. } = &err else {
            panic!("expected an ambiguous name, got {err}");
        };

        assert_eq!(candidates, &["feature-user-auth", "fix-auth"]);
    }

    /// A closer kind of fit ends the tie rather than joining it: the
    /// label is one workspace's in full, and only part of the other's.
    #[test]
    fn a_closer_fit_settles_what_a_looser_one_could_not() {
        assert_eq!(found("fix-auth"), "fix-auth");
    }

    #[test]
    fn a_name_that_fits_nothing_is_not_found() {
        let err = find_one("nope", &three()).expect_err("no such workspace");
        assert!(matches!(err, Error::WorkspaceNotFound(_)), "{err}");
    }

    fn listed(query: &str) -> Vec<String> {
        let records = three();
        candidates(query, &records)
            .iter()
            .map(|record| record.label.clone())
            .collect()
    }

    #[test]
    fn candidates_are_listed_closest_first() {
        assert_eq!(listed("auth"), ["feature-user-auth", "fix-auth"]);
    }

    /// What a completion asks before anything has been typed.
    #[test]
    fn an_empty_name_means_all_of_them() {
        assert_eq!(listed(""), ["feature-user-auth", "fix-auth", "main"]);
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
