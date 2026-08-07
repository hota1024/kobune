//! workspace のライフサイクル。
//!
//! 応答に人間向けの文字列を混ぜない。進捗はすべて [`EventSink`] に流し、
//! 表示の仕方は CLI と GUI がそれぞれ決める（`docs/DESIGN.md` §3）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use minato_api::{
    ApiError, ErrorCode, Pong, Request, Response, ServiceInfo, Target, WorkspaceInfo,
};
use minato_core::{MinatoConfig, Paths, ServiceScope, ServiceState, StateStore};
use minato_runtime::{EventSink, Runtime, ServiceStatus, WorkspaceKey};
use tokio::sync::Mutex;

use crate::resolve::{self, ProjectContext, Resolved};
use crate::spec;

pub struct Supervisor {
    store: StateStore,
    /// runtime は `[runtime] default` ごとに作って使い回す。
    /// プロジェクトによって別の runtime を使えるようにするため。
    runtimes: Mutex<HashMap<String, Arc<dyn Runtime>>>,
    /// 状態ファイルの更新を直列化する。
    state_lock: Mutex<()>,
    started_at: Instant,
    shutdown: Arc<tokio::sync::Notify>,
}

impl Supervisor {
    pub fn new(paths: &Paths, shutdown: Arc<tokio::sync::Notify>) -> Self {
        Self {
            store: StateStore::new(paths.state_file()),
            runtimes: Mutex::new(HashMap::new()),
            state_lock: Mutex::new(()),
            started_at: Instant::now(),
            shutdown,
        }
    }

    pub async fn handle(&self, request: Request, events: &EventSink) -> Result<Response, ApiError> {
        match request {
            Request::Ping => self.ping().await,
            Request::Shutdown => {
                self.shutdown.notify_waiters();
                Ok(Response::Empty)
            }
            Request::Ls {
                target,
                all_projects,
            } => self.ls(target, all_projects).await,
            Request::New {
                target,
                branch,
                base,
                path,
                start,
            } => {
                self.new_workspace(target, branch, base, path, start, events)
                    .await
            }
            Request::Rm { target, force } => self.rm(target, force, events).await,
            Request::Up { target, services } => self.up(target, services, events).await,
            Request::Down {
                target,
                services,
                all,
            } => self.down(target, services, all, events).await,
            Request::Status { target } => self.status(target).await,
        }
    }

    async fn ping(&self) -> Result<Response, ApiError> {
        // 既定の runtime に届くかを確かめ、届けばその版を返す。
        let (runtime_id, version) = match self.runtime("docker").await {
            Ok(runtime) => match runtime.probe().await {
                Ok(info) => (info.id, info.version),
                Err(_) => ("docker".to_string(), "unreachable".to_string()),
            },
            Err(_) => ("docker".to_string(), "unavailable".to_string()),
        };

        Ok(Response::Pong(Pong {
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol: minato_api::PROTOCOL_VERSION,
            runtime: format!("{runtime_id} {version}"),
            uptime_secs: self.started_at.elapsed().as_secs(),
        }))
    }

    /// runtime 実装を取得する。同じ識別子なら作り直さない。
    async fn runtime(&self, id: &str) -> Result<Arc<dyn Runtime>, ApiError> {
        let mut runtimes = self.runtimes.lock().await;

        if let Some(runtime) = runtimes.get(id) {
            return Ok(runtime.clone());
        }

        let runtime: Arc<dyn Runtime> = Arc::from(minato_runtime::create(id)?);
        runtimes.insert(id.to_string(), runtime.clone());
        Ok(runtime)
    }

    /// `cwd` からプロジェクトと workspace を解決する。
    async fn resolve(&self, target: &Target) -> Result<Resolved, ApiError> {
        let _guard = self.state_lock.lock().await;

        self.store
            .update(|state| {
                let context = resolve::resolve_project(target, state)?;
                context.resolve_workspace(target, state)
            })
            .map_err(ApiError::from)
    }

    /// プロジェクトだけを解決する（workspace がまだ無い操作向け）。
    async fn resolve_project_only(&self, target: &Target) -> Result<ProjectContext, ApiError> {
        let _guard = self.state_lock.lock().await;

        self.store
            .update(|state| resolve::resolve_project(target, state))
            .map_err(ApiError::from)
    }

    async fn ls(&self, target: Target, all_projects: bool) -> Result<Response, ApiError> {
        let context = self.resolve_project_only(&target).await?;
        let runtime = self.runtime(&context.config.runtime.default).await?;

        // 登録済みの worktree と、まだ登録していない worktree の両方を出す。
        // 「git worktree list には出るのに minato ls には出ない」を避ける。
        let worktrees = context.repo.worktrees().map_err(ApiError::from)?;

        let records = {
            let _guard = self.state_lock.lock().await;
            self.store
                .update(|state| {
                    let mut records = Vec::new();
                    for worktree in &worktrees {
                        if worktree.bare {
                            continue;
                        }
                        records.push(context.ensure_registered(worktree, state)?);
                    }
                    Ok(records)
                })
                .map_err(ApiError::from)?
        };

        // runtime への問い合わせは 1 回で済ませ、workspace ごとに振り分ける。
        let statuses = runtime.list_project(&context.project).await?;

        let mut workspaces = Vec::with_capacity(records.len());
        for record in records {
            workspaces.push(build_workspace_info(
                &context.config,
                &context.project,
                &record,
                &statuses,
            ));
        }

        if all_projects {
            // M0 では現在のプロジェクトのみ。全プロジェクト対応は
            // 状態ストアに他プロジェクトの設定パスを持たせてから。
            tracing::debug!("all_projects は M0 では現在のプロジェクトのみを返します");
        }

        Ok(Response::Workspaces { workspaces })
    }

    async fn status(&self, target: Target) -> Result<Response, ApiError> {
        let resolved = self.resolve(&target).await?;
        let runtime = self.runtime(&resolved.config.runtime.default).await?;
        let statuses = runtime.list_project(&resolved.project).await?;

        Ok(Response::Workspace {
            workspace: build_workspace_info(
                &resolved.config,
                &resolved.project,
                &resolved.workspace,
                &statuses,
            ),
        })
    }

    async fn new_workspace(
        &self,
        target: Target,
        branch: String,
        base: Option<String>,
        path: Option<PathBuf>,
        start: bool,
        events: &EventSink,
    ) -> Result<Response, ApiError> {
        let context = self.resolve_project_only(&target).await?;

        let worktree_path =
            path.unwrap_or_else(|| default_worktree_path(&context.repo.main_root, &branch));

        if worktree_path.exists() {
            return Err(ApiError::new(
                ErrorCode::AlreadyExists,
                format!("{} は既に存在します", worktree_path.display()),
            )
            .with_hint("別のパスを --path で指定するか、既存のディレクトリを削除してください"));
        }

        events.step_started("worktree", format!("worktree {branch} を作成"));
        context
            .repo
            .add_worktree(&worktree_path, &branch, base.as_deref())
            .map_err(|err| {
                events.step_failed(
                    "worktree",
                    format!("worktree {branch} を作成"),
                    err.to_string(),
                );
                ApiError::from(err)
            })?;
        events.step_done("worktree", format!("worktree {branch} を作成"));

        // 作った worktree を登録して、URL に使うラベルを確定させる。
        let record = {
            let _guard = self.state_lock.lock().await;
            let worktrees = context.repo.worktrees().map_err(ApiError::from)?;
            let created = worktrees
                .iter()
                .find(|wt| wt.path == worktree_path || wt.branch.as_deref() == Some(&branch))
                .cloned()
                .ok_or_else(|| ApiError::internal("作成した worktree を git が認識していません"))?;

            self.store
                .update(|state| context.register(&created, state))
                .map_err(ApiError::from)?
        };

        let resolved = Resolved {
            repo: context.repo,
            config: context.config,
            project: context.project,
            workspace: record,
        };

        if start {
            self.start_services(&resolved, &[], events).await?;
        }

        let runtime = self.runtime(&resolved.config.runtime.default).await?;
        let statuses = runtime.list_project(&resolved.project).await?;

        Ok(Response::Workspace {
            workspace: build_workspace_info(
                &resolved.config,
                &resolved.project,
                &resolved.workspace,
                &statuses,
            ),
        })
    }

    async fn rm(
        &self,
        target: Target,
        force: bool,
        events: &EventSink,
    ) -> Result<Response, ApiError> {
        let resolved = self.resolve(&target).await?;

        if resolved.workspace.is_main {
            return Err(ApiError::new(
                ErrorCode::InvalidConfig,
                "main worktree は削除できません".to_string(),
            )
            .with_hint("削除したい workspace を --workspace で指定してください"));
        }

        let runtime = self.runtime(&resolved.config.runtime.default).await?;
        let key = WorkspaceKey::new(&resolved.project, &resolved.workspace.label);

        runtime.destroy_workspace(&key, events).await?;

        events.step_started("worktree", "worktree を削除");
        resolved
            .repo
            .remove_worktree(&resolved.workspace.path, force)
            .map_err(|err| {
                events.step_failed("worktree", "worktree を削除", err.to_string());
                ApiError::from(err)
                    .with_hint("未コミットの変更が残っている場合は --force を付けてください")
            })?;
        events.step_done("worktree", "worktree を削除");

        {
            let _guard = self.state_lock.lock().await;
            self.store
                .update(|state| {
                    if let Some(project) = state.project_mut(&resolved.project) {
                        project.remove_workspace(&resolved.workspace.label);
                    }
                    Ok(())
                })
                .map_err(ApiError::from)?;
        }

        Ok(Response::Empty)
    }

    async fn up(
        &self,
        target: Target,
        services: Vec<String>,
        events: &EventSink,
    ) -> Result<Response, ApiError> {
        let resolved = self.resolve(&target).await?;
        self.start_services(&resolved, &services, events).await?;

        let runtime = self.runtime(&resolved.config.runtime.default).await?;
        let statuses = runtime.list_project(&resolved.project).await?;

        Ok(Response::Workspace {
            workspace: build_workspace_info(
                &resolved.config,
                &resolved.project,
                &resolved.workspace,
                &statuses,
            ),
        })
    }

    async fn start_services(
        &self,
        resolved: &Resolved,
        only: &[String],
        events: &EventSink,
    ) -> Result<(), ApiError> {
        let runtime = self.runtime(&resolved.config.runtime.default).await?;

        let workspace_spec = spec::build_workspace_spec(
            &resolved.config,
            &resolved.project,
            &resolved.workspace.label,
            &resolved.workspace.path,
        )?;

        // 対象を絞る場合も、依存先は一緒に起動する必要がある。
        let selected = select_with_dependencies(&resolved.config, only)?;

        let filtered: Vec<_> = workspace_spec
            .services
            .iter()
            .filter(|s| selected.contains(&s.name().to_string()))
            .cloned()
            .collect();

        if filtered.is_empty() {
            return Err(ApiError::new(
                ErrorCode::NotFound,
                "起動対象のサービスがありません".to_string(),
            ));
        }

        let prepare_spec = minato_runtime::WorkspaceSpec {
            key: workspace_spec.key.clone(),
            worktree_path: workspace_spec.worktree_path.clone(),
            services: filtered.clone(),
        };

        runtime.prepare(&prepare_spec, events).await?;

        // startup_order の順序を保ったまま起動する。
        for service in &filtered {
            runtime.start(service, events).await?;
        }

        Ok(())
    }

    async fn down(
        &self,
        target: Target,
        services: Vec<String>,
        all: bool,
        events: &EventSink,
    ) -> Result<Response, ApiError> {
        let resolved = self.resolve(&target).await?;
        let runtime = self.runtime(&resolved.config.runtime.default).await?;

        if all {
            // プロジェクト内の Minato 管理下のサービスをすべて止める。
            let statuses = runtime.list_project(&resolved.project).await?;
            for status in statuses {
                if status.state.is_running() {
                    runtime.stop(&status.key, events).await?;
                }
            }
        } else {
            let key = WorkspaceKey::new(&resolved.project, &resolved.workspace.label);

            // サービス名を明示したかどうかで共有サービスの扱いを変える。
            let explicit = !services.is_empty();
            let targets: Vec<String> = if explicit {
                validate_service_names(&resolved.config, &services)?;
                services
            } else {
                resolved.config.services.keys().cloned().collect()
            };

            for name in targets {
                let service_config = resolved.config.service(&name).map_err(ApiError::from)?;

                // 共有サービスは他の workspace も使っているため、
                // 名前を明示したときだけ止める。
                if service_config.scope == ServiceScope::Project && !explicit {
                    events.step_skipped(
                        "stop",
                        format!("{name} を停止"),
                        "共有サービスは明示指定した場合のみ停止します",
                    );
                    continue;
                }

                let service_key = match service_config.scope {
                    ServiceScope::Workspace => key.service(&name),
                    ServiceScope::Project => WorkspaceKey::shared(&resolved.project).service(&name),
                };

                runtime.stop(&service_key, events).await?;
            }
        }

        let statuses = runtime.list_project(&resolved.project).await?;

        Ok(Response::Workspace {
            workspace: build_workspace_info(
                &resolved.config,
                &resolved.project,
                &resolved.workspace,
                &statuses,
            ),
        })
    }
}

/// 指定されたサービスと、その依存先をまとめて返す。
fn select_with_dependencies(
    config: &MinatoConfig,
    only: &[String],
) -> Result<Vec<String>, ApiError> {
    if only.is_empty() {
        return Ok(config.services.keys().cloned().collect());
    }

    validate_service_names(config, only)?;

    let mut selected: Vec<String> = Vec::new();
    let mut stack: Vec<String> = only.to_vec();

    while let Some(name) = stack.pop() {
        if selected.contains(&name) {
            continue;
        }

        if let Some(service) = config.services.get(&name) {
            for dep in &service.depends_on {
                if !selected.contains(dep) {
                    stack.push(dep.clone());
                }
            }
        }

        selected.push(name);
    }

    Ok(selected)
}

fn validate_service_names(config: &MinatoConfig, names: &[String]) -> Result<(), ApiError> {
    for name in names {
        if !config.services.contains_key(name) {
            let available: Vec<&str> = config.services.keys().map(String::as_str).collect();
            return Err(
                ApiError::not_found(format!("サービス `{name}` は定義されていません"))
                    .with_hint(format!("利用できるサービス: {}", available.join(", "))),
            );
        }
    }
    Ok(())
}

/// worktree を作る既定のパス。
///
/// `{リポジトリ名}.wt/{ブランチ}` をリポジトリと同じ階層に置く。
/// リポジトリの中に作るとエディタや検索の対象に入ってしまう。
fn default_worktree_path(main_root: &std::path::Path, branch: &str) -> PathBuf {
    let repo_name = main_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".to_string());

    let parent = main_root.parent().unwrap_or(main_root);
    let label = minato_core::naming::sanitize_label(branch);

    parent.join(format!("{repo_name}.wt")).join(label)
}

/// 設定と runtime の状態を突き合わせて、クライアントに返す形にする。
fn build_workspace_info(
    config: &MinatoConfig,
    project: &str,
    record: &minato_core::WorkspaceRecord,
    statuses: &[ServiceStatus],
) -> WorkspaceInfo {
    let workspace_key = WorkspaceKey::new(project, &record.label);
    let shared_key = WorkspaceKey::shared(project);

    let services = config
        .services
        .iter()
        .map(|(name, service_config)| {
            let key = match service_config.scope {
                ServiceScope::Workspace => workspace_key.service(name),
                ServiceScope::Project => shared_key.service(name),
            };

            let status = statuses.iter().find(|s| s.key == key);

            ServiceInfo {
                name: name.clone(),
                state: status
                    .map(|s| s.state.clone())
                    .unwrap_or(ServiceState::Stopped),
                scope: service_config.scope,
                // URL はプロキシが動く M1 以降。
                url: None,
                tunnel_url: None,
                endpoint: status
                    .and_then(|s| s.endpoint)
                    .filter(|_| service_config.exposed())
                    .map(|addr| addr.to_string()),
                port: service_config.port,
                container_id: status.and_then(|s| s.container_id.clone()),
                image: status
                    .and_then(|s| s.image.clone())
                    .or_else(|| service_config.image.clone()),
            }
        })
        .collect();

    WorkspaceInfo {
        project: project.to_string(),
        workspace: record.url_label().map(str::to_string),
        branch: record.branch.clone(),
        path: record.path.clone(),
        is_main: record.is_main,
        services,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn config(toml: &str) -> MinatoConfig {
        let config: MinatoConfig = toml::from_str(toml).expect("構文は正しい");
        config.validate().expect("意味も正しい");
        config
    }

    const SAMPLE: &str = r#"
        [project]
        name = "myapp"
        [services.web]
        image = "node:22"
        port = 3000
        depends_on = ["api"]
        [services.api]
        image = "node:22"
        port = 8080
        depends_on = ["db"]
        [services.db]
        image = "postgres:16"
        port = 5432
        scope = "project"
        expose = false
    "#;

    #[test]
    fn selecting_a_service_pulls_in_its_dependencies() {
        let config = config(SAMPLE);
        let selected = select_with_dependencies(&config, &["web".to_string()]).expect("解決できる");

        assert!(selected.contains(&"web".to_string()));
        assert!(selected.contains(&"api".to_string()), "推移的な依存も含む");
        assert!(selected.contains(&"db".to_string()), "推移的な依存も含む");
    }

    #[test]
    fn empty_selection_means_everything() {
        let config = config(SAMPLE);
        let selected = select_with_dependencies(&config, &[]).expect("解決できる");
        assert_eq!(selected.len(), 3);
    }

    #[test]
    fn unknown_service_lists_the_available_ones() {
        let config = config(SAMPLE);
        let err = select_with_dependencies(&config, &["nope".to_string()]).unwrap_err();

        assert_eq!(err.code, ErrorCode::NotFound);
        let hint = err.hint.expect("hint がある");
        assert!(hint.contains("web") && hint.contains("api"), "got: {hint}");
    }

    #[test]
    fn worktree_path_sits_beside_the_repository() {
        let path =
            default_worktree_path(Path::new("/Users/x/ghq/github.com/y/myapp"), "feature/one");

        assert_eq!(
            path,
            PathBuf::from("/Users/x/ghq/github.com/y/myapp.wt/feature-one"),
            "リポジトリの中に作るとエディタの検索対象に入ってしまう"
        );
    }

    fn record(label: &str, is_main: bool) -> minato_core::WorkspaceRecord {
        minato_core::WorkspaceRecord {
            label: label.to_string(),
            branch: "feature/one".to_string(),
            path: PathBuf::from("/repo/wt/feat-1"),
            is_main,
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn reports_stopped_when_runtime_knows_nothing() {
        let info = build_workspace_info(&config(SAMPLE), "myapp", &record("feat-1", false), &[]);

        assert_eq!(info.services.len(), 3);
        for service in &info.services {
            assert_eq!(service.state, ServiceState::Stopped);
            assert_eq!(service.endpoint, None);
        }
    }

    #[test]
    fn matches_shared_services_against_the_shared_key() {
        let statuses = vec![ServiceStatus {
            key: WorkspaceKey::shared("myapp").service("db"),
            state: ServiceState::Ready,
            container_id: Some("abc".into()),
            image: Some("postgres:16".into()),
            endpoint: Some("127.0.0.1:5432".parse().expect("valid")),
            port: Some(5432),
            scope: ServiceScope::Project,
        }];

        let info = build_workspace_info(
            &config(SAMPLE),
            "myapp",
            &record("feat-1", false),
            &statuses,
        );
        let db = info.service("db").expect("存在する");

        assert_eq!(db.state, ServiceState::Ready, "共有サービスも状態が引ける");
        assert_eq!(
            db.endpoint, None,
            "expose = false なので待ち受け先は見せない"
        );
    }

    #[test]
    fn exposes_endpoint_for_published_services() {
        let statuses = vec![ServiceStatus {
            key: WorkspaceKey::new("myapp", "feat-1").service("web"),
            state: ServiceState::Ready,
            container_id: Some("abc".into()),
            image: Some("node:22".into()),
            endpoint: Some("127.0.0.1:49312".parse().expect("valid")),
            port: Some(3000),
            scope: ServiceScope::Workspace,
        }];

        let info = build_workspace_info(
            &config(SAMPLE),
            "myapp",
            &record("feat-1", false),
            &statuses,
        );
        let web = info.service("web").expect("存在する");

        assert_eq!(web.endpoint.as_deref(), Some("127.0.0.1:49312"));
        assert_eq!(web.access().as_deref(), Some("http://127.0.0.1:49312"));
    }

    #[test]
    fn main_workspace_has_no_url_label() {
        let info = build_workspace_info(&config(SAMPLE), "myapp", &record("main", true), &[]);

        assert_eq!(info.workspace, None, "main は URL から省略する");
        assert!(info.is_main);
        assert_eq!(info.display_name(), "(main)");
    }
}
