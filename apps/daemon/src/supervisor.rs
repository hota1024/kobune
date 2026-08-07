//! workspace のライフサイクル。
//!
//! 応答に人間向けの文字列を混ぜない。進捗はすべて [`EventSink`] に流し、
//! 表示の仕方は CLI と GUI がそれぞれ決める（`docs/DESIGN.md` §3）。

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use minato_api::{
    ApiError, Check, Diagnostics, EnvInfo, ErrorCode, Pong, Request, Response, ServiceInfo, Target,
    WorkspaceInfo,
};
use minato_core::{MinatoConfig, Paths, ServiceScope, ServiceState, StateStore, WorkspaceRecord};
use minato_proxy::{Activation, Route};
use minato_runtime::{EventSink, Runtime, ServiceStatus, WorkspaceKey};

/// アイドル判定でサービスをまとめる鍵（workspace, service）。
type ServiceKeyRef = (String, String);
use tokio::sync::Mutex;

use crate::env;
use crate::gateway::Gateway;
use crate::idle::IdleTracker;
use crate::resolve::{self, ProjectContext, Resolved};
use crate::secrets;
use crate::spec;

pub struct Supervisor {
    paths: Paths,
    store: StateStore,
    /// runtime は `[runtime] default` ごとに作って使い回す。
    /// プロジェクトによって別の runtime を使えるようにするため。
    runtimes: Mutex<HashMap<String, Arc<dyn Runtime>>>,
    /// 状態ファイルの更新を直列化する。
    state_lock: Mutex<()>,
    /// プロキシと DNS の入り口。URL の発行元でもある。
    gateway: Arc<Gateway>,
    /// 最終アクセス時刻。scale-to-zero の判断材料。
    idle: IdleTracker,
    started_at: Instant,
    shutdown: Arc<tokio::sync::Notify>,
}

impl Supervisor {
    pub fn new(paths: &Paths, gateway: Arc<Gateway>, shutdown: Arc<tokio::sync::Notify>) -> Self {
        Self {
            paths: paths.clone(),
            store: StateStore::new(paths.state_file()),
            runtimes: Mutex::new(HashMap::new()),
            state_lock: Mutex::new(()),
            gateway,
            idle: IdleTracker::new(),
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
            Request::Doctor => self.doctor().await,
            Request::EnvList { target, reveal } => self.env_list(target, reveal).await,
            Request::EnvSet {
                target,
                scope,
                key,
                value,
            } => self.env_set(target, scope, key, value).await,
            Request::EnvUnset { target, scope, key } => self.env_unset(target, scope, key).await,
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

    /// daemon 側で分かることを診断する。
    ///
    /// システム側の設定（`/etc/resolver`、CA の信頼）は daemon からは
    /// 判定しにくいので CLI が担当する。ここでは待ち受けと runtime を見る。
    async fn doctor(&self) -> Result<Response, ApiError> {
        let mut checks = Vec::new();

        // runtime に届くか。届かなければ何も起動できない。
        match self.runtime("docker").await {
            Ok(runtime) => match runtime.probe().await {
                Ok(info) => checks.push(Check::ok(
                    "runtime",
                    "コンテナランタイム",
                    format!("{} {}", info.id, info.version),
                )),
                Err(err) => checks.push(
                    Check::fail("runtime", "コンテナランタイム", err.to_string()).with_fix(
                        "Docker Desktop / OrbStack / colima のいずれかを起動してください",
                    ),
                ),
            },
            Err(err) => checks.push(Check::fail(
                "runtime",
                "コンテナランタイム",
                err.to_string(),
            )),
        }

        checks.push(match self.gateway.http_port() {
            Some(port) => Check::ok("proxy-http", "HTTP プロキシ", format!("127.0.0.1:{port}")),
            None => Check::fail(
                "proxy-http",
                "HTTP プロキシ",
                "待ち受けていません（ポートを確保できませんでした）".to_string(),
            )
            .with_fix(
                "1024 未満のポートには権限が要ります。`minato setup` の手順を実行するか、\
                 MINATO_HTTP_PORT で別のポートを指定してください",
            ),
        });

        // 片方のアドレス族しか取れていないと、もう片方に来た
        // リクエストは別のプロセスへ渡る。黙って進むと原因が掴めない。
        let missing = self.gateway.missing_families();
        if !missing.is_empty() {
            let families: Vec<String> = missing.iter().map(|ip| ip.to_string()).collect();
            checks.push(
                Check::fail(
                    "proxy-families",
                    "待ち受けアドレス",
                    format!(
                        "{} を確保できていません。*.localhost は両方に解決されるため、\
                         この宛先のリクエストは別のプロセスに渡ります",
                        families.join(", ")
                    ),
                )
                .with_fix(
                    "そのアドレスを使っている別のプロセスを止めるか、\
                     MINATO_HTTP_PORT / MINATO_HTTPS_PORT で空いているポートを指定してください",
                ),
            );
        }

        checks.push(match self.gateway.https_port() {
            Some(port) => Check::ok("proxy-https", "HTTPS プロキシ", format!("127.0.0.1:{port}")),
            None => Check::warn(
                "proxy-https",
                "HTTPS プロキシ",
                "待ち受けていません。HTTP のみ利用できます".to_string(),
            )
            .with_fix("MINATO_HTTPS_PORT で別のポートを指定してください"),
        });

        checks.push(match self.gateway.dns_port() {
            Some(port) => Check::ok("dns", "DNS サーバ", format!("127.0.0.1:{port}")),
            None => Check::fail(
                "dns",
                "DNS サーバ",
                "待ち受けていません。*.localhost を解決できません".to_string(),
            )
            .with_fix("MINATO_DNS_PORT で 1024 以上のポートを指定してください"),
        });

        // 特権ポートを使えているかは launchd から fd を受けたかで決まる。
        checks.push(if crate::activation::is_active() {
            Check::ok(
                "launchd",
                "launchd socket activation",
                "有効（特権ポートを利用できます）".to_string(),
            )
        } else {
            Check::warn(
                "launchd",
                "launchd socket activation",
                "無効。80/443 は使えないため、非標準ポートで待ち受けます".to_string(),
            )
            .with_fix("`minato setup` の手順で LaunchDaemon を設置してください")
        });

        checks.push(match self.gateway.ca_path() {
            Some(path) => Check::ok("ca", "ローカル CA", path.display().to_string()),
            None => Check::warn("ca", "ローカル CA", "生成されていません".to_string()),
        });

        Ok(Response::Diagnostics(Diagnostics::new(checks)))
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

    /// runtime の現状を取り直し、ルーティングを作り直す。
    ///
    /// 個々の変化を追わず毎回まるごと入れ替える。runtime のラベルを
    /// 状態の正とする方針と揃い、取りこぼしが起きない。
    async fn refresh(
        &self,
        project: &str,
        config: &MinatoConfig,
    ) -> Result<Vec<ServiceStatus>, ApiError> {
        let runtime = self.runtime(&config.runtime.default).await?;
        let statuses = runtime.list_project(project).await?;

        let records = self.workspace_records(project).await?;
        let entries = route_entries(config, project, &records, &statuses);

        // 起動中なのに記録が無いホスト（daemon 再起動後など）に基準を与える。
        // 記録が無いと永久にアイドル判定されず、止まらなくなる。
        for (host, route) in &entries {
            if route.is_running() && self.idle.idle_for(host).is_none() {
                self.idle.touch(host);
            }
        }

        self.gateway.routes().replace_project(project, entries);

        Ok(statuses)
    }

    /// 環境変数を層ごとに見せる。
    ///
    /// **どの層で定義された値かを出す。** 3 層あるので、意図しない層の
    /// 値が効いていることに気づけないと原因が掴めない。
    async fn env_list(&self, target: Target, reveal: bool) -> Result<Response, ApiError> {
        let resolved = self.resolve(&target).await?;

        // サービス個別の指定を含めない共通の層だけを見せる。
        // サービスごとの差分は `minato status` の領分。
        let layers = env::layers_for_service(
            &resolved.config,
            &resolved.project,
            &resolved.workspace,
            &resolved.repo.main_root,
            // 代表として最初のサービスを使う。MINATO_SERVICE 以外は共通。
            resolved
                .config
                .services
                .keys()
                .next()
                .map(String::as_str)
                .unwrap_or(""),
            &self.paths,
            &self.gateway,
        )
        .map_err(|err| ApiError::new(ErrorCode::InvalidConfig, err.to_string()))?;

        let entries = layers
            .resolve()
            .into_iter()
            .map(|entry| {
                let secret = entry.secret_ref();

                // 自動注入の値は Minato が作ったもので秘密ではない。
                // URL を確認したい場面は多いので伏せない。
                let injected = entry.scope == minato_core::EnvScope::Injected;

                EnvInfo {
                    key: entry.key,
                    value: if reveal || injected || secret.is_some() {
                        // シークレットは --reveal でも参照のままにする。
                        // 実体を出すには解決が要り、それは起動時にだけ行う。
                        entry.raw.clone()
                    } else {
                        minato_core::env::mask(&entry.raw)
                    },
                    scope: entry.scope,
                    secret: secret.is_some(),
                    source: secret.map(|reference| reference.describe()),
                }
            })
            .collect();

        Ok(Response::Env { entries })
    }

    async fn env_set(
        &self,
        target: Target,
        scope: minato_core::EnvScope,
        key: String,
        value: String,
    ) -> Result<Response, ApiError> {
        if !minato_core::env::is_valid_key(&key) {
            return Err(ApiError::new(
                ErrorCode::InvalidConfig,
                format!("`{key}` は環境変数名として使えません"),
            )
            .with_hint("英数字とアンダースコアのみ、先頭は数字以外にしてください"));
        }

        let path = self.env_file_path(&target, scope).await?;
        let current = read_or_empty(&path)?;

        minato_core::env::write_file(&path, &minato_core::env::upsert(&current, &key, &value))
            .map_err(|err| ApiError::internal(err.to_string()))?;

        self.env_list(target, false).await
    }

    async fn env_unset(
        &self,
        target: Target,
        scope: minato_core::EnvScope,
        key: String,
    ) -> Result<Response, ApiError> {
        let path = self.env_file_path(&target, scope).await?;
        let current = read_or_empty(&path)?;

        minato_core::env::write_file(&path, &minato_core::env::remove(&current, &key))
            .map_err(|err| ApiError::internal(err.to_string()))?;

        self.env_list(target, false).await
    }

    /// 層に対応するファイルの場所。
    async fn env_file_path(
        &self,
        target: &Target,
        scope: minato_core::EnvScope,
    ) -> Result<PathBuf, ApiError> {
        if !scope.is_writable() {
            return Err(ApiError::new(
                ErrorCode::InvalidConfig,
                format!("{} の値はファイルに書けません", scope.label()),
            ));
        }

        if scope == minato_core::EnvScope::Global {
            return Ok(self.paths.root().join(minato_core::env::GLOBAL_ENV_FILE));
        }

        let resolved = self.resolve(target).await?;

        Ok(match scope {
            minato_core::EnvScope::Project => {
                minato_core::env::project_env_path(&resolved.repo.main_root)
            }
            _ => minato_core::env::workspace_env_path(&resolved.workspace.path),
        })
    }

    /// サービスに渡す環境変数を確定させる。
    ///
    /// 層を重ね、シークレット参照を解決する。**解決値はここから先も
    /// ディスクに書かない。** コンテナに渡すためだけに使う。
    async fn service_env(
        &self,
        config: &MinatoConfig,
        project: &str,
        record: &WorkspaceRecord,
        project_root: &std::path::Path,
        service: &str,
        events: &EventSink,
    ) -> Result<BTreeMap<String, String>, ApiError> {
        let layers = env::layers_for_service(
            config,
            project,
            record,
            project_root,
            service,
            &self.paths,
            &self.gateway,
        )
        .map_err(|err| ApiError::new(ErrorCode::InvalidConfig, err.to_string()))?;

        let entries = layers.resolve();

        // 参照とそれ以外に分ける。
        let mut values = BTreeMap::new();
        let mut references = Vec::new();

        for entry in entries {
            match entry.secret_ref() {
                Some(reference) => references.push((entry.key, reference)),
                None => {
                    values.insert(entry.key, entry.raw);
                }
            }
        }

        if references.is_empty() {
            return Ok(values);
        }

        let resolved = secrets::resolve(&references).await;
        values.extend(resolved.values);

        // 解決できなかったものは落とすが、黙って落とさない。
        // 気づかないまま「なぜか認証に失敗する」状態になるのが最悪。
        for (key, reason) in resolved.failures {
            events.warn(format!("{key} のシークレットを解決できません: {reason}"));
            tracing::warn!("{service}: {key} のシークレットを解決できません: {reason}");
        }

        Ok(values)
    }

    /// workspace の全サービス分の環境変数。
    async fn workspace_envs(
        &self,
        config: &MinatoConfig,
        project: &str,
        record: &WorkspaceRecord,
        project_root: &std::path::Path,
        events: &EventSink,
    ) -> Result<BTreeMap<String, BTreeMap<String, String>>, ApiError> {
        let mut envs = BTreeMap::new();

        for name in config.services.keys() {
            let values = self
                .service_env(config, project, record, project_root, name, events)
                .await?;
            envs.insert(name.clone(), values);
        }

        Ok(envs)
    }

    /// アクセスがあったことを記録する。プロキシから毎リクエスト呼ばれる。
    pub fn touch(&self, host: &str) {
        self.idle.touch(host);
    }

    /// 停止しているサービスを起こす。
    ///
    /// `wait` を過ぎても受け付けられない場合は [`Activation::Starting`] を
    /// 返すが、**起動処理は続く**。呼び出し側が待ち直せば繋がる。
    pub async fn activate(&self, host: &str, wait: Duration) -> Activation {
        let Some(route) = self.gateway.routes().get(host) else {
            return Activation::Unknown;
        };

        if let Some(endpoint) = route.endpoint {
            self.idle.touch(host);
            return Activation::Ready(endpoint);
        }

        // 同じホストに同時にリクエストが来ても起動は 1 回だけ。
        // 取れなかった側は、先行する起動の完了を待つ。
        match self.idle.begin_start(host) {
            Some(guard) => {
                let outcome = self.start_for_host(host, &route).await;
                drop(guard);

                match outcome {
                    Ok(Some(endpoint)) => {
                        self.idle.touch(host);
                        Activation::Ready(endpoint)
                    }
                    // 起動はしたが待ち受け先が分からない（ポート未公開など）。
                    Ok(None) => Activation::Starting,
                    Err(err) => Activation::Failed(err.message),
                }
            }
            None => self.await_route(host, wait).await,
        }
    }

    /// ルートに endpoint が現れるまで待つ。既に別の起動が走っている場合に使う。
    async fn await_route(&self, host: &str, wait: Duration) -> Activation {
        let deadline = tokio::time::Instant::now() + wait;

        loop {
            if let Some(route) = self.gateway.routes().get(host) {
                if let Some(endpoint) = route.endpoint {
                    self.idle.touch(host);
                    return Activation::Ready(endpoint);
                }
            }

            if tokio::time::Instant::now() >= deadline {
                return Activation::Starting;
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// ホストに対応するサービスを 1 つ起動する。
    async fn start_for_host(
        &self,
        host: &str,
        route: &Route,
    ) -> Result<Option<SocketAddr>, ApiError> {
        let (config, record) = self.locate(&route.project, &route.workspace).await?;

        let service_config = config.service(&route.service).map_err(ApiError::from)?;
        let events = EventSink::discard();

        let project_root = self.project_root(&route.project).await?;
        let service_env = self
            .service_env(
                &config,
                &route.project,
                &record,
                &project_root,
                &route.service,
                &events,
            )
            .await?;

        let service_spec = spec::build_service_spec(
            service_config,
            &route.service,
            &route.project,
            &record.label,
            &record.path,
            service_env,
            config.services.keys().cloned().collect(),
        )?;

        let runtime = self.runtime(&config.runtime.default).await?;

        // イメージが無いと start が失敗する。単体のサービスとして用意する。
        let workspace_spec = minato_runtime::WorkspaceSpec {
            key: WorkspaceKey::new(&route.project, &record.label),
            worktree_path: record.path.clone(),
            services: vec![service_spec.clone()],
        };
        runtime.prepare(&workspace_spec, &events).await?;

        tracing::info!("{host} へのアクセスにより {} を起動します", route.service);
        let running = runtime.start(&service_spec, &events).await?;

        self.refresh(&route.project, &config).await?;
        Ok(running.endpoint)
    }

    /// プロジェクトと workspace ラベルから、設定と登録情報を引く。
    async fn locate(
        &self,
        project: &str,
        workspace: &str,
    ) -> Result<(MinatoConfig, WorkspaceRecord), ApiError> {
        let record = {
            let _guard = self.state_lock.lock().await;
            let state = self.store.load().map_err(ApiError::from)?;

            state
                .project(project)
                .and_then(|p| p.workspace(workspace))
                .cloned()
                .ok_or_else(|| {
                    ApiError::not_found(format!("workspace `{workspace}` の登録がありません"))
                })?
        };

        let (_, config) = MinatoConfig::find(&record.path).map_err(ApiError::from)?;
        Ok((config, record))
    }

    /// 状態ストアに記録されたプロジェクトの root。
    async fn project_root(&self, project: &str) -> Result<PathBuf, ApiError> {
        let _guard = self.state_lock.lock().await;
        let state = self.store.load().map_err(ApiError::from)?;

        state
            .project(project)
            .map(|record| record.root.clone())
            .ok_or_else(|| {
                ApiError::not_found(format!("プロジェクト `{project}` の登録がありません"))
            })
    }

    /// プロジェクトの設定を、状態ストアに記録された root から読む。
    async fn project_config(&self, project: &str) -> Result<MinatoConfig, ApiError> {
        let root = {
            let _guard = self.state_lock.lock().await;
            let state = self.store.load().map_err(ApiError::from)?;

            state
                .project(project)
                .map(|record| record.root.clone())
                .ok_or_else(|| {
                    ApiError::not_found(format!("プロジェクト `{project}` の登録がありません"))
                })?
        };

        let (_, config) = MinatoConfig::find(&root).map_err(ApiError::from)?;
        Ok(config)
    }

    /// アイドルなサービスを止める。監視タスクから定期的に呼ばれる。
    ///
    /// 止めた数を返す。
    pub async fn sweep_idle(&self) -> usize {
        let snapshot = self.gateway.routes().snapshot();
        if snapshot.is_empty() {
            return 0;
        }

        let mut projects: BTreeMap<String, Vec<(String, Route)>> = BTreeMap::new();
        for (host, route) in snapshot {
            projects
                .entry(route.project.clone())
                .or_default()
                .push((host, route));
        }

        let mut stopped = 0;
        for (project, routes) in projects {
            match self.sweep_project(&project, &routes).await {
                Ok(count) => stopped += count,
                Err(err) => tracing::debug!("{project} のアイドル判定に失敗しました: {err}"),
            }
        }

        stopped
    }

    async fn sweep_project(
        &self,
        project: &str,
        routes: &[(String, Route)],
    ) -> Result<usize, ApiError> {
        let config = self.project_config(project).await?;
        let runtime = self.runtime(&config.runtime.default).await?;
        let events = EventSink::discard();

        // 共有サービスは複数の workspace から参照される。1 つでも
        // 使われていれば止められないので、サービス単位でまとめて判断する。
        let mut by_service: BTreeMap<ServiceKeyRef, Vec<&(String, Route)>> = BTreeMap::new();
        for entry in routes {
            let (_, route) = entry;
            if !route.is_running() {
                continue;
            }

            let Ok(service_config) = config.service(&route.service) else {
                continue;
            };

            let key = match service_config.scope {
                ServiceScope::Workspace => (route.workspace.clone(), route.service.clone()),
                // scope = project では workspace を無視して 1 つに畳む。
                ServiceScope::Project => (String::new(), route.service.clone()),
            };

            by_service.entry(key).or_default().push(entry);
        }

        let mut stopped = 0;
        for ((workspace, service), entries) in by_service {
            let Ok(service_config) = config.service(&service) else {
                continue;
            };
            let timeout = service_config.idle_timeout();

            // 参照しているホストが 1 つでも生きていれば止めない。
            let all_idle = entries
                .iter()
                .all(|(host, _)| self.idle.idle_for(host).is_some_and(|idle| idle >= timeout));

            if !all_idle {
                continue;
            }

            let service_key = match service_config.scope {
                ServiceScope::Workspace => WorkspaceKey::new(project, &workspace).service(&service),
                ServiceScope::Project => WorkspaceKey::shared(project).service(&service),
            };

            tracing::info!(
                "{service_key} を停止します（{} アクセスがありません）",
                humantime::format_duration(timeout)
            );

            if let Err(err) = runtime.stop(&service_key, &events).await {
                tracing::warn!("{service_key} を停止できませんでした: {err}");
                continue;
            }

            for (host, _) in entries {
                self.idle.forget(host);
            }
            stopped += 1;
        }

        if stopped > 0 {
            self.refresh(project, &config).await?;
        }

        Ok(stopped)
    }

    /// 状態ストアに登録済みの workspace 一覧。
    async fn workspace_records(&self, project: &str) -> Result<Vec<WorkspaceRecord>, ApiError> {
        let _guard = self.state_lock.lock().await;

        let state = self.store.load().map_err(ApiError::from)?;
        Ok(state
            .project(project)
            .map(|record| record.workspaces.values().cloned().collect())
            .unwrap_or_default())
    }

    async fn ls(&self, target: Target, all_projects: bool) -> Result<Response, ApiError> {
        let context = self.resolve_project_only(&target).await?;

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
        let statuses = self.refresh(&context.project, &context.config).await?;

        let mut workspaces = Vec::with_capacity(records.len());
        for record in records {
            workspaces.push(self.build_workspace_info(
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
        let statuses = self.refresh(&resolved.project, &resolved.config).await?;

        Ok(Response::Workspace {
            workspace: self.build_workspace_info(
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

        let statuses = self.refresh(&resolved.project, &resolved.config).await?;

        Ok(Response::Workspace {
            workspace: self.build_workspace_info(
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

        let statuses = self.refresh(&resolved.project, &resolved.config).await?;

        Ok(Response::Workspace {
            workspace: self.build_workspace_info(
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

        let envs = self
            .workspace_envs(
                &resolved.config,
                &resolved.project,
                &resolved.workspace,
                &resolved.repo.main_root,
                events,
            )
            .await?;

        let workspace_spec = spec::build_workspace_spec(
            &resolved.config,
            &resolved.project,
            &resolved.workspace.label,
            &resolved.workspace.path,
            &envs,
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

        let statuses = self.refresh(&resolved.project, &resolved.config).await?;

        Ok(Response::Workspace {
            workspace: self.build_workspace_info(
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

/// ファイルを読む。無ければ空文字。
fn read_or_empty(path: &std::path::Path) -> Result<String, ApiError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(ApiError::internal(format!(
            "{} を読めません: {err}",
            path.display()
        ))),
    }
}

/// プロキシに登録するホスト名と転送先の一覧を作る。
///
/// **停止中のサービスも載せる。** scale-to-zero では「止まっている」ことと
/// 「存在しない」ことを区別する必要がある。前者はリクエストで起こし、
/// 後者は 404 を返す。転送先は起動しているものにだけ入れる。
fn route_entries(
    config: &MinatoConfig,
    project: &str,
    records: &[WorkspaceRecord],
    statuses: &[ServiceStatus],
) -> Vec<(String, Route)> {
    let domain = config.domain();
    let shared_key = WorkspaceKey::shared(project);
    let mut entries = Vec::new();

    for record in records {
        let workspace_key = WorkspaceKey::new(project, &record.label);

        for (name, service_config) in &config.services {
            if !service_config.exposed() {
                continue;
            }

            let key = match service_config.scope {
                ServiceScope::Workspace => workspace_key.service(name),
                ServiceScope::Project => shared_key.service(name),
            };

            let status = statuses.iter().find(|s| s.key == key);
            let endpoint = status
                .filter(|status| status.state.is_running())
                .and_then(|status| status.endpoint);

            let host = minato_core::naming::service_host_in(name, record.url_label(), &domain);
            let route = match endpoint {
                Some(endpoint) => Route::new(endpoint, project, &record.label, name.clone()),
                None => Route::stopped(project, &record.label, name.clone()),
            };

            entries.push((host, route));
        }
    }

    entries
}

impl Supervisor {
    /// 設定と runtime の状態を突き合わせて、クライアントに返す形にする。
    fn build_workspace_info(
        &self,
        config: &MinatoConfig,
        project: &str,
        record: &WorkspaceRecord,
        statuses: &[ServiceStatus],
    ) -> WorkspaceInfo {
        let workspace_key = WorkspaceKey::new(project, &record.label);
        let shared_key = WorkspaceKey::shared(project);
        let domain = config.domain();

        let services = config
            .services
            .iter()
            .map(|(name, service_config)| {
                let key = match service_config.scope {
                    ServiceScope::Workspace => workspace_key.service(name),
                    ServiceScope::Project => shared_key.service(name),
                };

                let status = statuses.iter().find(|s| s.key == key);
                let state = status
                    .map(|s| s.state.clone())
                    .unwrap_or(ServiceState::Stopped);

                // 停止中でも URL は出す。アクセスすれば起動するので、
                // 案内先として正しい。URL が状態で消えると
                // 「さっきまで動いていた URL が無くなった」と見える。
                let url = if service_config.exposed() {
                    let host =
                        minato_core::naming::service_host_in(name, record.url_label(), &domain);
                    self.gateway.url_for(&host)
                } else {
                    None
                };

                ServiceInfo {
                    name: name.clone(),
                    state,
                    scope: service_config.scope,
                    url,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::Gateway;
    use std::net::SocketAddr;
    use std::path::Path;

    /// URL の組み立てとルーティングだけを見るための Supervisor。
    /// 状態ストアには触れないので、実体のないパスで構わない。
    fn supervisor(gateway: Gateway) -> Supervisor {
        Supervisor::new(
            &Paths::with_root(PathBuf::from("/tmp/minato-supervisor-test")),
            Arc::new(gateway),
            Arc::new(tokio::sync::Notify::new()),
        )
    }

    fn ready(key: minato_runtime::ServiceKey, port: u16, scope: ServiceScope) -> ServiceStatus {
        ServiceStatus {
            key,
            state: ServiceState::Ready,
            container_id: Some("abc".into()),
            image: Some("busybox".into()),
            endpoint: Some(SocketAddr::from(([127, 0, 0, 1], port))),
            port: Some(port),
            scope,
        }
    }

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
        let info = supervisor(Gateway::with_ports(Some(80), Some(443))).build_workspace_info(
            &config(SAMPLE),
            "myapp",
            &record("feat-1", false),
            &[],
        );

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

        let info = supervisor(Gateway::inert()).build_workspace_info(
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

        let info = supervisor(Gateway::inert()).build_workspace_info(
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
        let info = supervisor(Gateway::inert()).build_workspace_info(
            &config(SAMPLE),
            "myapp",
            &record("main", true),
            &[],
        );

        assert_eq!(info.workspace, None, "main は URL から省略する");
        assert!(info.is_main);
        assert_eq!(info.display_name(), "(main)");
    }

    #[test]
    fn issues_urls_when_the_proxy_is_listening() {
        let statuses = vec![ready(
            WorkspaceKey::new("myapp", "feat-1").service("web"),
            49312,
            ServiceScope::Workspace,
        )];

        let info = supervisor(Gateway::with_ports(Some(80), Some(443))).build_workspace_info(
            &config(SAMPLE),
            "myapp",
            &record("feat-1", false),
            &statuses,
        );

        let web = info.service("web").expect("存在する");
        assert_eq!(
            web.url.as_deref(),
            Some("https://web.feat-1.myapp.localhost")
        );
        assert_eq!(
            web.access().as_deref(),
            Some("https://web.feat-1.myapp.localhost"),
            "URL があるならそちらを案内する"
        );
    }

    #[test]
    fn issues_no_url_when_the_proxy_is_down() {
        // 待ち受けていないのに URL を返すと、繋がらない先を教えることになる。
        let statuses = vec![ready(
            WorkspaceKey::new("myapp", "feat-1").service("web"),
            49312,
            ServiceScope::Workspace,
        )];

        let info = supervisor(Gateway::inert()).build_workspace_info(
            &config(SAMPLE),
            "myapp",
            &record("feat-1", false),
            &statuses,
        );

        let web = info.service("web").expect("存在する");
        assert_eq!(web.url, None);
        assert_eq!(
            web.access().as_deref(),
            Some("http://127.0.0.1:49312"),
            "URL が無くてもポート直指定は案内できる"
        );
    }

    #[test]
    fn main_workspace_url_omits_the_label() {
        let statuses = vec![ready(
            WorkspaceKey::new("myapp", "main").service("web"),
            49312,
            ServiceScope::Workspace,
        )];

        let info = supervisor(Gateway::with_ports(Some(80), Some(443))).build_workspace_info(
            &config(SAMPLE),
            "myapp",
            &record("main", true),
            &statuses,
        );

        assert_eq!(
            info.service("web").expect("存在する").url.as_deref(),
            Some("https://web.myapp.localhost")
        );
    }

    #[test]
    fn stopped_services_still_advertise_their_url() {
        // scale-to-zero ではアクセスが起動のきっかけになる。URL が
        // 消えると、起こす手段そのものが見えなくなる。
        let info = supervisor(Gateway::with_ports(Some(80), Some(443))).build_workspace_info(
            &config(SAMPLE),
            "myapp",
            &record("feat-1", false),
            &[],
        );

        let web = info.service("web").expect("存在する");
        assert_eq!(web.state, ServiceState::Stopped);
        assert_eq!(
            web.url.as_deref(),
            Some("https://web.feat-1.myapp.localhost")
        );

        // 公開しないサービスは停止中でも URL を持たない。
        assert_eq!(info.service("db").expect("存在する").url, None);
    }

    #[test]
    fn routes_only_running_exposed_services() {
        let records = vec![record("feat-1", false)];
        let statuses = vec![
            ready(
                WorkspaceKey::new("myapp", "feat-1").service("web"),
                49312,
                ServiceScope::Workspace,
            ),
            // expose = false なので URL もルートも作らない。
            ready(
                WorkspaceKey::shared("myapp").service("db"),
                5432,
                ServiceScope::Project,
            ),
            // 停止中は転送先にしない。502 を返すだけになる。
            ServiceStatus {
                key: WorkspaceKey::new("myapp", "feat-1").service("api"),
                state: ServiceState::Stopped,
                container_id: None,
                image: None,
                endpoint: None,
                port: Some(8080),
                scope: ServiceScope::Workspace,
            },
        ];

        let entries = route_entries(&config(SAMPLE), "myapp", &records, &statuses);

        let running: Vec<&str> = entries
            .iter()
            .filter(|(_, route)| route.is_running())
            .map(|(host, _)| host.as_str())
            .collect();
        assert_eq!(running, vec!["web.feat-1.myapp.localhost"]);

        // 停止中の api も「存在する」ものとして登録され、
        // リクエストが来たら起こせる。
        let stopped: Vec<&str> = entries
            .iter()
            .filter(|(_, route)| !route.is_running())
            .map(|(host, _)| host.as_str())
            .collect();
        assert_eq!(stopped, vec!["api.feat-1.myapp.localhost"]);

        // expose = false の db はどちらにも現れない。
        assert!(!entries.iter().any(|(host, _)| host.starts_with("db.")));

        let web = entries
            .iter()
            .find(|(host, _)| host.starts_with("web."))
            .expect("ある");
        assert_eq!(web.1.endpoint.expect("起動中").port(), 49312);
    }

    #[test]
    fn routes_every_workspace_of_the_project() {
        let records = vec![record("feat-1", false), record("main", true)];
        let statuses = vec![
            ready(
                WorkspaceKey::new("myapp", "feat-1").service("web"),
                49312,
                ServiceScope::Workspace,
            ),
            ready(
                WorkspaceKey::new("myapp", "main").service("web"),
                49313,
                ServiceScope::Workspace,
            ),
        ];

        let entries = route_entries(&config(SAMPLE), "myapp", &records, &statuses);
        let mut hosts: Vec<&str> = entries
            .iter()
            .filter(|(_, route)| route.is_running())
            .map(|(host, _)| host.as_str())
            .collect();
        hosts.sort();

        assert_eq!(
            hosts,
            vec!["web.feat-1.myapp.localhost", "web.myapp.localhost"]
        );
    }
}
