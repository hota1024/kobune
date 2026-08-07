//! Docker バックエンド。
//!
//! `docker compose` CLI は呼ばず、Docker API を直接叩く。compose に乗ると
//! 「compose にできること」が仕様の上限になり、他の runtime と揃わなくなるため。
//!
//! ポートはホストの `127.0.0.1` に動的割り当てでフォワードする。
//! `0.0.0.0` に晒すと同じ LAN の他人から開発環境が見えてしまう。

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use async_trait::async_trait;
use bollard::Docker;
use bollard::container::{
    Config, CreateContainerOptions, ListContainersOptions, LogOutput, LogsOptions,
    RemoveContainerOptions, StartContainerOptions, StopContainerOptions,
};
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::image::CreateImageOptions;
use bollard::models::{
    ContainerSummary, EndpointSettings, HostConfig, Mount, MountTypeEnum, PortBinding,
};
use bollard::network::{ConnectNetworkOptions, CreateNetworkOptions, ListNetworksOptions};
use futures::StreamExt;
use futures::stream::BoxStream;
use minato_api::OutputStream;
use minato_core::{ServiceScope, ServiceState};

use crate::error::{Result, RuntimeError};
use crate::event::EventSink;
use crate::health::{DEFAULT_READINESS_TIMEOUT, await_service};
use crate::runtime::{ExecOutcome, LogLine, LogOptions, Runtime, RuntimeInfo, labels, names};
use crate::spec::{
    RunningService, ServiceKey, ServiceSpec, ServiceStatus, SourceMount, VolumeMount, WorkspaceKey,
    WorkspaceSpec,
};

const RUNTIME_ID: &str = "docker";

/// 停止時に SIGKILL へ切り替えるまでの秒数。
const STOP_TIMEOUT_SECS: i64 = 10;

pub struct DockerRuntime {
    docker: Docker,
}

impl DockerRuntime {
    /// 環境変数と既定のソケットから接続先を決める。
    ///
    /// この時点では通信しないため、Docker が起動していなくても成功する。
    /// 実際の到達性は [`Runtime::probe`] で確かめる。
    pub fn connect() -> Result<Self> {
        let docker =
            Docker::connect_with_local_defaults().map_err(|err| RuntimeError::Unavailable {
                runtime: RUNTIME_ID.to_string(),
                message: err.to_string(),
            })?;

        Ok(Self { docker })
    }

    pub fn with_client(docker: Docker) -> Self {
        Self { docker }
    }

    fn unavailable(err: impl std::fmt::Display) -> RuntimeError {
        RuntimeError::Unavailable {
            runtime: RUNTIME_ID.to_string(),
            message: err.to_string(),
        }
    }

    /// ネットワークがなければ作る。
    async fn ensure_network(&self, key: &WorkspaceKey) -> Result<String> {
        let name = names::network(key);

        let mut filters = HashMap::new();
        filters.insert("name".to_string(), vec![name.clone()]);

        let existing = self
            .docker
            .list_networks(Some(ListNetworksOptions { filters }))
            .await
            .map_err(|e| RuntimeError::failed("ネットワークの一覧取得", e))?;

        // 前方一致で返るため、名前が完全に一致するものだけを見る。
        if existing
            .iter()
            .any(|n| n.name.as_deref() == Some(name.as_str()))
        {
            return Ok(name);
        }

        let mut network_labels = HashMap::new();
        network_labels.insert(
            labels::MANAGED.to_string(),
            labels::MANAGED_VALUE.to_string(),
        );
        network_labels.insert(labels::PROJECT.to_string(), key.project.clone());
        network_labels.insert(labels::WORKSPACE.to_string(), key.workspace.clone());

        self.docker
            .create_network(CreateNetworkOptions {
                name: name.clone(),
                driver: "bridge".to_string(),
                labels: network_labels,
                ..Default::default()
            })
            .await
            .map_err(|e| RuntimeError::failed("ネットワークの作成", e))?;

        Ok(name)
    }

    /// イメージがローカルになければ取得する。
    async fn ensure_image(&self, image: &str, events: &EventSink) -> Result<()> {
        if self.docker.inspect_image(image).await.is_ok() {
            events.step_skipped("pull", format!("イメージ {image}"), "既に存在します");
            return Ok(());
        }

        events.step_started("pull", format!("イメージ {image} を取得"));

        let mut stream = self.docker.create_image(
            Some(CreateImageOptions {
                from_image: image,
                ..Default::default()
            }),
            None,
            None,
        );

        while let Some(item) = stream.next().await {
            match item {
                Ok(info) => {
                    if let Some(status) = info.status {
                        events.step_progress("pull", format!("イメージ {image} を取得"), status);
                    }
                }
                Err(err) => {
                    events.step_failed("pull", format!("イメージ {image} を取得"), err.to_string());
                    return Err(RuntimeError::ImageUnavailable {
                        image: image.to_string(),
                        message: err.to_string(),
                    });
                }
            }
        }

        events.step_done("pull", format!("イメージ {image} を取得"));
        Ok(())
    }

    /// 名前付きボリュームを用意する。
    async fn ensure_volume(&self, project: &str, name: &str) -> Result<String> {
        let full = names::volume(project, name);

        if self.docker.inspect_volume(&full).await.is_ok() {
            return Ok(full);
        }

        let mut volume_labels = HashMap::new();
        volume_labels.insert(
            labels::MANAGED.to_string(),
            labels::MANAGED_VALUE.to_string(),
        );
        volume_labels.insert(labels::PROJECT.to_string(), project.to_string());

        self.docker
            .create_volume(bollard::volume::CreateVolumeOptions {
                name: full.clone(),
                labels: volume_labels,
                ..Default::default()
            })
            .await
            .map_err(|e| RuntimeError::failed("ボリュームの作成", e))?;

        Ok(full)
    }

    /// コンテナが存在すればそのサマリを返す。
    async fn find_container(&self, key: &ServiceKey) -> Result<Option<ContainerSummary>> {
        let mut filters = HashMap::new();
        filters.insert(
            "label".to_string(),
            vec![
                format!("{}={}", labels::PROJECT, key.workspace.project),
                format!("{}={}", labels::WORKSPACE, key.workspace.workspace),
                format!("{}={}", labels::SERVICE, key.service),
            ],
        );

        let containers = self
            .docker
            .list_containers(Some(ListContainersOptions {
                all: true,
                filters,
                ..Default::default()
            }))
            .await
            .map_err(Self::unavailable)?;

        Ok(containers.into_iter().next())
    }

    /// コンテナを作る。既存があれば作り直す。
    async fn create_container(&self, spec: &ServiceSpec, network: &str) -> Result<String> {
        let name = names::container(&spec.key);
        let port = spec.port;

        let mut container_labels = HashMap::new();
        container_labels.insert(
            labels::MANAGED.to_string(),
            labels::MANAGED_VALUE.to_string(),
        );
        container_labels.insert(
            labels::PROJECT.to_string(),
            spec.key.workspace.project.clone(),
        );
        container_labels.insert(
            labels::WORKSPACE.to_string(),
            spec.key.workspace.workspace.clone(),
        );
        container_labels.insert(labels::SERVICE.to_string(), spec.key.service.clone());
        container_labels.insert(
            labels::SCOPE.to_string(),
            match spec.scope {
                ServiceScope::Workspace => "workspace".to_string(),
                ServiceScope::Project => "project".to_string(),
            },
        );
        if let Some(port) = port {
            container_labels.insert(labels::PORT.to_string(), port.to_string());
        }

        // ホスト側ポートを空にすると Docker が空きポートを選ぶ。
        // 127.0.0.1 に限定して LAN には出さない。
        let mut port_bindings = HashMap::new();
        let mut exposed_ports = HashMap::new();
        if let Some(port) = port {
            let key = format!("{port}/tcp");
            port_bindings.insert(
                key.clone(),
                Some(vec![PortBinding {
                    host_ip: Some("127.0.0.1".to_string()),
                    host_port: Some(String::new()),
                }]),
            );
            exposed_ports.insert(key, HashMap::new());
        }

        let mut mounts = Vec::new();
        if let Some(SourceMount { host, target }) = &spec.source_mount {
            mounts.push(Mount {
                typ: Some(MountTypeEnum::BIND),
                source: Some(host.to_string_lossy().to_string()),
                target: Some(target.clone()),
                ..Default::default()
            });
        }

        for volume in &spec.volumes {
            match volume {
                VolumeMount::Named {
                    name,
                    target,
                    read_only,
                } => {
                    let full = self
                        .ensure_volume(&spec.key.workspace.project, name)
                        .await?;
                    mounts.push(Mount {
                        typ: Some(MountTypeEnum::VOLUME),
                        source: Some(full),
                        target: Some(target.clone()),
                        read_only: Some(*read_only),
                        ..Default::default()
                    });
                }
                VolumeMount::Bind {
                    source,
                    target,
                    read_only,
                } => {
                    mounts.push(Mount {
                        typ: Some(MountTypeEnum::BIND),
                        source: Some(source.to_string_lossy().to_string()),
                        target: Some(target.clone()),
                        read_only: Some(*read_only),
                        ..Default::default()
                    });
                }
            }
        }

        // サービス名でも引けるようにする。`api` から `db:5432` で繋がる。
        let mut endpoints = HashMap::new();
        endpoints.insert(
            network.to_string(),
            EndpointSettings {
                aliases: Some(vec![spec.key.service.clone()]),
                ..Default::default()
            },
        );

        let config = Config {
            image: Some(spec.image.clone()),
            cmd: spec.command.clone(),
            working_dir: Some(spec.workdir.clone()),
            env: Some(spec.env_pairs()),
            labels: Some(container_labels),
            exposed_ports: if exposed_ports.is_empty() {
                None
            } else {
                Some(exposed_ports)
            },
            host_config: Some(HostConfig {
                mounts: if mounts.is_empty() {
                    None
                } else {
                    Some(mounts)
                },
                port_bindings: if port_bindings.is_empty() {
                    None
                } else {
                    Some(port_bindings)
                },
                ..Default::default()
            }),
            networking_config: Some(bollard::container::NetworkingConfig {
                endpoints_config: endpoints,
            }),
            ..Default::default()
        };

        let created = self
            .docker
            .create_container(
                Some(CreateContainerOptions {
                    name: name.clone(),
                    platform: None,
                }),
                config,
            )
            .await
            .map_err(|e| RuntimeError::failed(format!("コンテナ {name} の作成"), e))?;

        Ok(created.id)
    }

    /// 起動中のコンテナからホスト側の待ち受けアドレスを取り出す。
    async fn resolve_endpoint(
        &self,
        container_id: &str,
        port: Option<u16>,
    ) -> Result<Option<SocketAddr>> {
        let Some(port) = port else {
            return Ok(None);
        };

        let details = self
            .docker
            .inspect_container(container_id, None)
            .await
            .map_err(|e| RuntimeError::failed("コンテナの状態取得", e))?;

        let bindings = details
            .network_settings
            .and_then(|settings| settings.ports)
            .and_then(|ports| ports.get(&format!("{port}/tcp")).cloned())
            .flatten();

        let host_port = bindings
            .and_then(|list| list.into_iter().next())
            .and_then(|binding| binding.host_port)
            .and_then(|value| value.parse::<u16>().ok());

        Ok(host_port.map(|p| SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), p)))
    }

    fn summary_to_status(summary: &ContainerSummary) -> Option<ServiceStatus> {
        let labels_map = summary.labels.as_ref()?;

        let project = labels_map.get(labels::PROJECT)?.clone();
        let workspace = labels_map.get(labels::WORKSPACE)?.clone();
        let service = labels_map.get(labels::SERVICE)?.clone();

        let scope = match labels_map.get(labels::SCOPE).map(String::as_str) {
            Some("project") => ServiceScope::Project,
            _ => ServiceScope::Workspace,
        };

        let port = labels_map
            .get(labels::PORT)
            .and_then(|value| value.parse::<u16>().ok());

        // `docker ps` の state は文字列。running 以外は停止扱いにする。
        let state = match summary.state.as_deref() {
            Some("running") => ServiceState::Ready,
            Some("created") | Some("restarting") => ServiceState::Starting,
            Some("exited") | Some("dead") | Some("paused") | Some("removing") => {
                ServiceState::Stopped
            }
            _ => ServiceState::Unknown,
        };

        // 公開ポートからホスト側アドレスを拾う。
        let endpoint = summary
            .ports
            .as_ref()
            .and_then(|ports| {
                ports.iter().find(|p| match (p.private_port, port) {
                    (private, Some(expected)) => private == expected,
                    (_, None) => false,
                })
            })
            .and_then(|p| p.public_port)
            .map(|p| SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), p));

        Some(ServiceStatus {
            key: WorkspaceKey::new(project, workspace).service(service),
            state,
            container_id: summary.id.clone(),
            image: summary.image.clone(),
            endpoint,
            port,
            scope,
        })
    }
}

#[async_trait]
impl Runtime for DockerRuntime {
    fn id(&self) -> &'static str {
        RUNTIME_ID
    }

    async fn probe(&self) -> Result<RuntimeInfo> {
        let version = self.docker.version().await.map_err(Self::unavailable)?;

        Ok(RuntimeInfo {
            id: RUNTIME_ID.to_string(),
            version: version.version.unwrap_or_else(|| "unknown".to_string()),
            supports_custom_networks: true,
        })
    }

    async fn prepare(&self, spec: &WorkspaceSpec, events: &EventSink) -> Result<()> {
        events.step_started("network", "ネットワークを用意");
        self.ensure_network(&spec.key).await?;

        // 共有サービスがいる場合、共有ネットワークも用意しておく。
        if spec
            .services
            .iter()
            .any(|s| s.scope == ServiceScope::Project)
        {
            self.ensure_network(&WorkspaceKey::shared(&spec.key.project))
                .await?;
        }
        events.step_done("network", "ネットワークを用意");

        for service in &spec.services {
            self.ensure_image(&service.image, events).await?;
        }

        Ok(())
    }

    async fn start(&self, spec: &ServiceSpec, events: &EventSink) -> Result<RunningService> {
        let name = names::container(&spec.key);

        // 既に動いていれば何もしない。`minato up` を何度叩いても同じ結果になる。
        if let Some(existing) = self.find_container(&spec.key).await? {
            let id = existing.id.clone().unwrap_or_default();

            if existing.state.as_deref() == Some("running") {
                events.step_skipped(
                    "start",
                    format!("{} を起動", spec.name()),
                    "既に起動しています",
                );
                let endpoint = self.resolve_endpoint(&id, spec.port).await?;
                events.service_state(spec.name(), ServiceState::Ready);

                return Ok(RunningService {
                    key: spec.key.clone(),
                    container_id: id,
                    endpoint,
                });
            }

            // 停止中のコンテナは設定が古い可能性があるので作り直す。
            // 起動が数秒遅くなるが、設定変更が反映されない方が混乱を招く。
            self.docker
                .remove_container(
                    &id,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await
                .map_err(|e| RuntimeError::failed(format!("コンテナ {name} の削除"), e))?;
        }

        events.step_started("start", format!("{} を起動", spec.name()));
        events.service_state(spec.name(), ServiceState::Starting);

        let network = names::network(&spec.attached_to);
        self.ensure_network(&spec.attached_to).await?;

        let id = self.create_container(spec, &network).await?;

        // 共有サービスは呼び出し元の workspace ネットワークにも参加させる。
        if spec.scope == ServiceScope::Project {
            let shared = self
                .ensure_network(&WorkspaceKey::shared(&spec.key.workspace.project))
                .await?;

            let _ = self
                .docker
                .connect_network(
                    &shared,
                    ConnectNetworkOptions {
                        container: id.clone(),
                        endpoint_config: EndpointSettings {
                            aliases: Some(vec![spec.key.service.clone()]),
                            ..Default::default()
                        },
                    },
                )
                .await;
        }

        self.docker
            .start_container(&id, None::<StartContainerOptions<String>>)
            .await
            .map_err(|e| {
                events.step_failed("start", format!("{} を起動", spec.name()), e.to_string());
                RuntimeError::failed(format!("コンテナ {name} の起動"), e)
            })?;

        let endpoint = self.resolve_endpoint(&id, spec.port).await?;

        events.step_done("start", format!("{} を起動", spec.name()));

        // コンテナが動き出しても、中のアプリはまだ listen していないことがある。
        // ここで待たないと `minato new` 直後の curl が connection refused になる。
        await_service(
            spec.name(),
            endpoint,
            spec.health.as_ref(),
            DEFAULT_READINESS_TIMEOUT,
            events,
        )
        .await;

        events.service_state(spec.name(), ServiceState::Ready);

        Ok(RunningService {
            key: spec.key.clone(),
            container_id: id,
            endpoint,
        })
    }

    async fn stop(&self, key: &ServiceKey, events: &EventSink) -> Result<()> {
        let Some(container) = self.find_container(key).await? else {
            events.step_skipped(
                "stop",
                format!("{} を停止", key.service),
                "起動していません",
            );
            return Ok(());
        };

        let id = container.id.unwrap_or_default();
        if container.state.as_deref() != Some("running") {
            events.step_skipped(
                "stop",
                format!("{} を停止", key.service),
                "起動していません",
            );
            return Ok(());
        }

        events.step_started("stop", format!("{} を停止", key.service));

        self.docker
            .stop_container(
                &id,
                Some(StopContainerOptions {
                    t: STOP_TIMEOUT_SECS,
                }),
            )
            .await
            .map_err(|e| RuntimeError::failed(format!("コンテナ {id} の停止"), e))?;

        events.step_done("stop", format!("{} を停止", key.service));
        events.service_state(&key.service, ServiceState::Stopped);
        Ok(())
    }

    async fn remove(&self, key: &ServiceKey, events: &EventSink) -> Result<()> {
        let Some(container) = self.find_container(key).await? else {
            return Ok(());
        };

        let id = container.id.unwrap_or_default();
        events.step_started("remove", format!("{} を削除", key.service));

        self.docker
            .remove_container(
                &id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| RuntimeError::failed(format!("コンテナ {id} の削除"), e))?;

        events.step_done("remove", format!("{} を削除", key.service));
        Ok(())
    }

    async fn destroy_workspace(&self, key: &WorkspaceKey, events: &EventSink) -> Result<()> {
        let mut filters = HashMap::new();
        filters.insert(
            "label".to_string(),
            vec![
                format!("{}={}", labels::PROJECT, key.project),
                format!("{}={}", labels::WORKSPACE, key.workspace),
            ],
        );

        let containers = self
            .docker
            .list_containers(Some(ListContainersOptions {
                all: true,
                filters,
                ..Default::default()
            }))
            .await
            .map_err(Self::unavailable)?;

        events.step_started("destroy", "コンテナを削除");
        for container in containers {
            if let Some(id) = container.id {
                self.docker
                    .remove_container(
                        &id,
                        Some(RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await
                    .map_err(|e| RuntimeError::failed(format!("コンテナ {id} の削除"), e))?;
            }
        }
        events.step_done("destroy", "コンテナを削除");

        // ネットワークは他に参加者がいなければ消える。失敗しても致命的ではない。
        let network = names::network(key);
        if let Err(err) = self.docker.remove_network(&network).await {
            events.debug(format!(
                "ネットワーク {network} は削除しませんでした: {err}"
            ));
        }

        Ok(())
    }

    async fn inspect(&self, key: &ServiceKey) -> Result<ServiceStatus> {
        match self.find_container(key).await? {
            Some(summary) => Ok(Self::summary_to_status(&summary)
                .unwrap_or_else(|| ServiceStatus::stopped(key.clone(), ServiceScope::Workspace))),
            None => Ok(ServiceStatus::stopped(key.clone(), ServiceScope::Workspace)),
        }
    }

    async fn logs(
        &self,
        key: &ServiceKey,
        options: LogOptions,
    ) -> Result<BoxStream<'static, LogLine>> {
        let container = self.find_container(key).await?.ok_or_else(|| {
            RuntimeError::failed(
                format!("{} のログ取得", key.service),
                "コンテナが存在しません",
            )
        })?;

        let id = container.id.unwrap_or_default();
        let stream = self.docker.logs(
            &id,
            Some(LogsOptions::<String> {
                follow: options.follow,
                stdout: true,
                stderr: true,
                tail: options
                    .tail
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "all".to_string()),
                ..Default::default()
            }),
        );

        // Docker は 1 チャンクに複数行を詰めてくることがある。行に割る。
        let lines = stream.flat_map(|item| {
            let chunk = match item {
                Ok(output) => output,
                Err(err) => {
                    tracing::debug!("ログの読み取りが終了しました: {err}");
                    return futures::stream::iter(Vec::new());
                }
            };

            let (stream_kind, bytes) = match chunk {
                LogOutput::StdErr { message } => (OutputStream::Stderr, message),
                LogOutput::StdOut { message }
                | LogOutput::Console { message }
                | LogOutput::StdIn { message } => (OutputStream::Stdout, message),
            };

            let text = String::from_utf8_lossy(&bytes).to_string();
            let lines: Vec<LogLine> = text
                .lines()
                .map(|line| LogLine {
                    stream: stream_kind,
                    line: line.to_string(),
                })
                .collect();

            futures::stream::iter(lines)
        });

        Ok(Box::pin(lines))
    }

    async fn exec(
        &self,
        key: &ServiceKey,
        command: &[String],
        events: &EventSink,
    ) -> Result<ExecOutcome> {
        let container = self.find_container(key).await?.ok_or_else(|| {
            RuntimeError::failed(
                format!("{} でのコマンド実行", key.service),
                "コンテナが存在しません。`minato up` で起動してください",
            )
        })?;

        if container.state.as_deref() != Some("running") {
            return Err(RuntimeError::failed(
                format!("{} でのコマンド実行", key.service),
                "コンテナが起動していません。`minato up` で起動してください",
            ));
        }

        let id = container.id.unwrap_or_default();
        let created = self
            .docker
            .create_exec(
                &id,
                CreateExecOptions {
                    cmd: Some(command.to_vec()),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    // TTY は要求しない。対話を待って固まる方が危険。
                    tty: Some(false),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| RuntimeError::failed("exec の作成", e))?;

        let started = self
            .docker
            .start_exec(&created.id, None)
            .await
            .map_err(|e| RuntimeError::failed("exec の開始", e))?;

        if let StartExecResults::Attached { mut output, .. } = started {
            while let Some(chunk) = output.next().await {
                let Ok(chunk) = chunk else { break };

                let (stream_kind, bytes) = match chunk {
                    LogOutput::StdErr { message } => (OutputStream::Stderr, message),
                    LogOutput::StdOut { message }
                    | LogOutput::Console { message }
                    | LogOutput::StdIn { message } => (OutputStream::Stdout, message),
                };

                for line in String::from_utf8_lossy(&bytes).lines() {
                    events.output(Some(key.service.clone()), stream_kind, line);
                }
            }
        }

        let inspected = self
            .docker
            .inspect_exec(&created.id)
            .await
            .map_err(|e| RuntimeError::failed("exec の状態取得", e))?;

        Ok(ExecOutcome {
            exit_code: inspected.exit_code.unwrap_or(-1) as i32,
        })
    }

    async fn list_project(&self, project: &str) -> Result<Vec<ServiceStatus>> {
        let mut filters = HashMap::new();
        filters.insert(
            "label".to_string(),
            vec![format!("{}={}", labels::PROJECT, project)],
        );

        let containers = self
            .docker
            .list_containers(Some(ListContainersOptions {
                all: true,
                filters,
                ..Default::default()
            }))
            .await
            .map_err(Self::unavailable)?;

        Ok(containers
            .iter()
            .filter_map(Self::summary_to_status)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::Port;
    use std::collections::HashMap;

    fn summary_with(labels_map: HashMap<String, String>, state: &str) -> ContainerSummary {
        ContainerSummary {
            id: Some("abc123".into()),
            image: Some("node:22".into()),
            state: Some(state.into()),
            labels: Some(labels_map),
            ports: Some(vec![Port {
                private_port: 3000,
                public_port: Some(49312),
                ip: Some("127.0.0.1".into()),
                typ: None,
            }]),
            ..Default::default()
        }
    }

    fn minato_labels() -> HashMap<String, String> {
        HashMap::from([
            (labels::MANAGED.to_string(), "1".to_string()),
            (labels::PROJECT.to_string(), "myapp".to_string()),
            (labels::WORKSPACE.to_string(), "feat-1".to_string()),
            (labels::SERVICE.to_string(), "web".to_string()),
            (labels::SCOPE.to_string(), "workspace".to_string()),
            (labels::PORT.to_string(), "3000".to_string()),
        ])
    }

    #[test]
    fn reconstructs_state_from_labels() {
        // daemon が再起動しても、この復元だけで状態が戻せる必要がある。
        let status = DockerRuntime::summary_to_status(&summary_with(minato_labels(), "running"))
            .expect("Minato のラベルが揃っていれば復元できる");

        assert_eq!(status.key.workspace.project, "myapp");
        assert_eq!(status.key.workspace.workspace, "feat-1");
        assert_eq!(status.key.service, "web");
        assert_eq!(status.state, ServiceState::Ready);
        assert_eq!(status.port, Some(3000));
        assert_eq!(status.scope, ServiceScope::Workspace);
        assert_eq!(
            status.endpoint,
            Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49312))
        );
    }

    #[test]
    fn ignores_containers_without_minato_labels() {
        let mut foreign = HashMap::new();
        foreign.insert("com.example.app".to_string(), "other".to_string());

        assert!(
            DockerRuntime::summary_to_status(&summary_with(foreign, "running")).is_none(),
            "他人のコンテナを拾ってはいけない"
        );
    }

    #[test]
    fn maps_docker_states() {
        let cases = [
            ("running", ServiceState::Ready),
            ("created", ServiceState::Starting),
            ("restarting", ServiceState::Starting),
            ("exited", ServiceState::Stopped),
            ("dead", ServiceState::Stopped),
            ("paused", ServiceState::Stopped),
        ];

        for (docker_state, expected) in cases {
            let status =
                DockerRuntime::summary_to_status(&summary_with(minato_labels(), docker_state))
                    .expect("復元できる");
            assert_eq!(status.state, expected, "docker state = {docker_state}");
        }
    }

    #[test]
    fn recognises_shared_scope() {
        let mut shared = minato_labels();
        shared.insert(labels::SCOPE.to_string(), "project".to_string());
        shared.insert(labels::WORKSPACE.to_string(), "_shared".to_string());

        let status =
            DockerRuntime::summary_to_status(&summary_with(shared, "running")).expect("復元できる");

        assert_eq!(status.scope, ServiceScope::Project);
        assert!(status.key.workspace.is_shared());
    }

    #[test]
    fn endpoint_is_absent_when_port_not_published() {
        let mut no_port = minato_labels();
        no_port.remove(labels::PORT);

        let mut summary = summary_with(no_port, "running");
        summary.ports = None;

        let status = DockerRuntime::summary_to_status(&summary).expect("復元できる");
        assert_eq!(status.port, None);
        assert_eq!(status.endpoint, None);
    }
}
