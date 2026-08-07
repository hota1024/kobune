//! workspace レジストリ。
//!
//! **実行中の状態はここに持たない。** コンテナの生死やポート割り当ては
//! runtime 側のラベル（`dev.minato.*`）を正とし、daemon が再起動しても
//! そこから復元する。このストアが持つのは「どの worktree を Minato が
//! 管理しているか」と「その worktree に発行した URL ラベル」だけ。
//!
//! ラベルを永続化するのは、[`crate::naming`] の規則を将来変更しても
//! 既存 workspace の URL が変わらないようにするため。

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::naming;

/// 状態ファイルのスキーマバージョン。
pub const CURRENT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub version: u32,

    /// キーはプロジェクト名（`minato.toml` の `[project] name`）。
    ///
    /// 名前が URL に現れる以上、名前が衝突したプロジェクトは共存できない。
    /// したがって名前をそのまま識別子として使い、衝突は登録時に弾く。
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectRecord>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            projects: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRecord {
    pub name: String,
    /// main worktree のパス。
    pub root: PathBuf,
    /// キーは URL に使う workspace ラベル。
    #[serde(default)]
    pub workspaces: BTreeMap<String, WorkspaceRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceRecord {
    /// URL に現れる名前。いったん発行したら変えない。
    pub label: String,
    /// 元のブランチ名（サニタイズ前）。
    pub branch: String,
    pub path: PathBuf,
    /// main worktree に対応する workspace かどうか。URL からラベルを省く。
    #[serde(default)]
    pub is_main: bool,
    pub created_at: DateTime<Utc>,
}

impl State {
    /// プロジェクトを登録する。既に別のパスで同名が登録されていればエラー。
    pub fn upsert_project(&mut self, name: &str, root: &Path) -> Result<&mut ProjectRecord> {
        match self.projects.get(name) {
            Some(existing) if existing.root != root => {
                return Err(Error::ConfigInvalid(format!(
                    "プロジェクト名 `{name}` は既に {} に登録されています。\
                     URL が衝突するため、どちらかの [project] name を変更してください",
                    existing.root.display()
                )));
            }
            _ => {}
        }

        Ok(self
            .projects
            .entry(name.to_string())
            .or_insert_with(|| ProjectRecord {
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
    /// URL に埋め込む workspace ラベル。
    ///
    /// main worktree では省略し、`{service}.{project}.localhost` になる。
    pub fn url_label(&self) -> Option<&str> {
        if self.is_main {
            None
        } else {
            Some(&self.label)
        }
    }
}

impl ProjectRecord {
    /// パスから workspace を引く。
    pub fn workspace_by_path(&self, path: &Path) -> Option<&WorkspaceRecord> {
        self.workspaces.values().find(|ws| ws.path == path)
    }

    pub fn workspace(&self, label: &str) -> Option<&WorkspaceRecord> {
        self.workspaces.get(label)
    }

    /// ブランチ名から、まだ使われていない workspace ラベルを決める。
    ///
    /// 既に同じブランチが登録されていればそのラベルを返す（べき等）。
    pub fn allocate_label(&self, branch: &str) -> String {
        if let Some(existing) = self.workspaces.values().find(|ws| ws.branch == branch) {
            return existing.label.clone();
        }

        let base = naming::sanitize_label(branch);
        if !self.workspaces.contains_key(&base) {
            return base;
        }

        // サニタイズで別のブランチと同じ形になった場合。
        // ブランチ名から決まるので、同じ衝突なら常に同じラベルになる。
        let disambiguated = naming::disambiguate(&base, branch);
        if !self.workspaces.contains_key(&disambiguated) {
            return disambiguated;
        }

        // ここに来るのはハッシュまで衝突した場合のみ。連番で逃がす。
        for n in 2..1000 {
            let candidate = naming::disambiguate(&base, &format!("{branch}#{n}"));
            if !self.workspaces.contains_key(&candidate) {
                return candidate;
            }
        }

        unreachable!("1000 通り試して空きがないことは実質起こらない");
    }

    pub fn insert_workspace(&mut self, record: WorkspaceRecord) {
        self.workspaces.insert(record.label.clone(), record);
    }

    pub fn remove_workspace(&mut self, label: &str) -> Option<WorkspaceRecord> {
        self.workspaces.remove(label)
    }
}

/// 状態ファイルの読み書き。
///
/// プロセス間の排他はしない。daemon が単一プロセスであることを
/// PID ファイルで担保し、daemon 内では `Mutex` で直列化する前提。
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

    /// 状態を読み込む。ファイルがなければ空の状態を返す。
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
                "状態ファイル {} のバージョン {} は、この minato (対応バージョン {}) では読めません。\
                 minato を更新してください",
                self.path.display(),
                state.version,
                CURRENT_VERSION
            )));
        }

        Ok(state)
    }

    /// 状態を書き出す。書き込み途中でのクラッシュで壊れないよう、
    /// 同一ディレクトリの一時ファイルに書いてから rename する。
    pub fn save(&self, state: &State) -> Result<()> {
        let dir = self.path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(dir).map_err(|source| Error::StateIo {
            path: dir.to_path_buf(),
            source,
        })?;

        let json = serde_json::to_vec_pretty(state).expect("State は常にシリアライズできる");

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

    /// 読み込み・変更・書き出しをまとめて行う。
    /// クロージャがエラーを返した場合は書き出さない。
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
        }
    }

    fn project() -> ProjectRecord {
        ProjectRecord {
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
        // `feature/x` と `feature_x` はサニタイズすると同じ形になる。
        p.insert_workspace(record("feature-x", "feature/x"));

        let label = p.allocate_label("feature_x");
        assert_ne!(label, "feature-x");
        assert!(naming::is_valid_label(&label), "got: {label}");

        // 決定的であること。
        assert_eq!(label, p.allocate_label("feature_x"));
    }

    #[test]
    fn rejects_same_project_name_at_different_root() {
        let mut state = State::default();
        state
            .upsert_project("myapp", Path::new("/repo/a"))
            .expect("初回は成功する");

        let err = state
            .upsert_project("myapp", Path::new("/repo/b"))
            .unwrap_err();
        assert!(err.to_string().contains("既に"), "got: {err}");
    }

    #[test]
    fn upsert_is_idempotent_for_same_root() {
        let mut state = State::default();
        state
            .upsert_project("myapp", Path::new("/repo"))
            .expect("ok");
        state
            .upsert_project("myapp", Path::new("/repo"))
            .expect("同じパスなら通る");
        assert_eq!(state.projects.len(), 1);
    }

    #[test]
    fn load_returns_default_when_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = StateStore::new(dir.path().join("state.json"));

        let state = store.load().expect("読める");
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
            .expect("書ける");

        let loaded = store.load().expect("読める");
        let project = loaded.project("myapp").expect("存在する");
        assert_eq!(project.root, PathBuf::from("/repo"));

        let ws = project.workspace("feat-1").expect("存在する");
        assert_eq!(ws.branch, "feature/one");
    }

    #[test]
    fn update_does_not_write_on_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = StateStore::new(dir.path().join("state.json"));

        let result: Result<()> = store.update(|state| {
            state.upsert_project("myapp", Path::new("/repo"))?;
            Err(Error::ConfigInvalid("途中で失敗".into()))
        });
        assert!(result.is_err());

        assert!(
            store.load().expect("読める").projects.is_empty(),
            "失敗した更新は永続化されない"
        );
    }

    #[test]
    fn rejects_future_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");
        std::fs::write(&path, r#"{"version": 999, "projects": {}}"#).expect("書ける");

        let err = StateStore::new(path).load().unwrap_err();
        assert!(err.to_string().contains("minato を更新"), "got: {err}");
    }

    #[test]
    fn reports_corrupt_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");
        std::fs::write(&path, "not json").expect("書ける");

        let err = StateStore::new(path).load().unwrap_err();
        assert!(err.to_string().contains("壊れています"), "got: {err}");
    }
}
