//! `minato.toml` の設定を runtime に渡す仕様へ変換する。
//!
//! runtime は `minato.toml` を知らない。設定の解釈はすべてここで済ませ、
//! runtime には解決済みの値だけを渡す。

use std::collections::BTreeMap;
use std::path::Path;

use minato_api::ApiError;
use minato_core::config::MOUNT_TARGET;
use minato_core::{MinatoConfig, ServiceConfig, ServiceScope};
use minato_runtime::{ServiceSpec, SourceMount, VolumeMount, WorkspaceKey, WorkspaceSpec};

/// workspace 全体の仕様を組み立てる。
pub fn build_workspace_spec(
    config: &MinatoConfig,
    project: &str,
    workspace: &str,
    worktree_path: &Path,
) -> Result<WorkspaceSpec, ApiError> {
    let key = WorkspaceKey::new(project, workspace);

    // 依存関係を満たす順に並べる。runtime はこの順で起動する。
    let ordered = config.startup_order();
    let mut services = Vec::with_capacity(ordered.len());

    for name in ordered {
        let service_config = config
            .services
            .get(name)
            .expect("startup_order は既存のサービス名だけを返す");

        services.push(build_service_spec(
            config,
            service_config,
            name,
            project,
            workspace,
            worktree_path,
        )?);
    }

    Ok(WorkspaceSpec {
        key,
        worktree_path: worktree_path.to_path_buf(),
        services,
    })
}

/// サービス 1 つの仕様を組み立てる。
pub fn build_service_spec(
    config: &MinatoConfig,
    service: &ServiceConfig,
    name: &str,
    project: &str,
    workspace: &str,
    worktree_path: &Path,
) -> Result<ServiceSpec, ApiError> {
    // M0 では既製イメージのみ。Dockerfile のビルドは M0.5 で対応する。
    let image = match (&service.image, &service.build) {
        (Some(image), _) => image.clone(),
        (None, Some(_)) => {
            return Err(ApiError::unsupported(format!(
                "サービス `{name}`: build による イメージのビルドは未対応です。\
                 image で既製イメージを指定してください"
            )));
        }
        (None, None) => {
            return Err(ApiError::unsupported(format!(
                "サービス `{name}`: image が指定されていません"
            )));
        }
    };

    let command = match &service.command {
        Some(raw) => Some(shell_words::split(raw).map_err(|err| {
            ApiError::new(
                minato_api::ErrorCode::InvalidConfig,
                format!("サービス `{name}`: command を解釈できません: {err}"),
            )
        })?),
        None => None,
    };

    let attached_to = WorkspaceKey::new(project, workspace);

    // 共有サービスは特定の worktree に属さない。
    let key = match service.scope {
        ServiceScope::Workspace => attached_to.service(name),
        ServiceScope::Project => WorkspaceKey::shared(project).service(name),
    };

    // 共有サービスに worktree のソースをマウントすると、
    // どの worktree の内容を見せるべきか決まらない。
    let source_mount = match service.scope {
        ServiceScope::Workspace => Some(SourceMount {
            host: worktree_path.to_path_buf(),
            target: MOUNT_TARGET.to_string(),
        }),
        ServiceScope::Project => None,
    };

    let mut volumes = Vec::with_capacity(service.volumes.len());
    for raw in &service.volumes {
        volumes.push(VolumeMount::parse(raw, worktree_path).map_err(|message| {
            ApiError::new(
                minato_api::ErrorCode::InvalidConfig,
                format!("サービス `{name}`: {message}"),
            )
        })?);
    }

    // 同じ workspace の他サービス。名前解決の手段を runtime が用意する。
    let peers: Vec<String> = config
        .services
        .keys()
        .filter(|other| other.as_str() != name)
        .cloned()
        .collect();

    Ok(ServiceSpec {
        key,
        attached_to,
        image,
        command,
        workdir: service.workdir().to_string(),
        env: build_env(service, name, project, workspace),
        port: service.port,
        scope: service.scope,
        volumes,
        source_mount,
        peers,
    })
}

/// サービスに渡す環境変数。
///
/// 自分がどの workspace で動いているかをアプリ側から知れるようにする。
/// URL の注入（`MINATO_URL_*`）はプロキシが動く M1 以降。
fn build_env(
    service: &ServiceConfig,
    name: &str,
    project: &str,
    workspace: &str,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();

    env.insert("MINATO_PROJECT".to_string(), project.to_string());
    env.insert("MINATO_WORKSPACE".to_string(), workspace.to_string());
    env.insert("MINATO_SERVICE".to_string(), name.to_string());

    // 利用者が書いた値を後から入れて、自動注入を上書きできるようにする。
    for (key, value) in &service.env {
        env.insert(key.clone(), value.clone());
    }

    env
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
        command = "pnpm dev"
        depends_on = ["db"]
        env = { NODE_ENV = "development" }

        [services.db]
        image = "postgres:16"
        port = 5432
        scope = "project"
        expose = false
        volumes = ["pgdata:/var/lib/postgresql/data"]
    "#;

    fn build() -> WorkspaceSpec {
        build_workspace_spec(
            &config(SAMPLE),
            "myapp",
            "feat-1",
            Path::new("/repo/wt/feat-1"),
        )
        .expect("組み立てられる")
    }

    #[test]
    fn orders_services_by_dependency() {
        let spec = build();
        let names: Vec<&str> = spec.services.iter().map(|s| s.name()).collect();
        assert_eq!(names, vec!["db", "web"], "依存先が先に来る");
    }

    #[test]
    fn splits_command_respecting_quotes() {
        let config = config(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            command = "sh -c 'echo hello world'"
        "#,
        );

        let spec = build_workspace_spec(&config, "myapp", "feat-1", Path::new("/repo"))
            .expect("組み立てられる");

        assert_eq!(
            spec.services[0].command,
            Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo hello world".to_string()
            ]),
            "引用符の中は 1 つの引数として扱う"
        );
    }

    #[test]
    fn mounts_worktree_for_workspace_scoped_services() {
        let spec = build();
        let web = spec.service("web").expect("存在する");

        assert_eq!(
            web.source_mount,
            Some(SourceMount {
                host: PathBuf::from("/repo/wt/feat-1"),
                target: MOUNT_TARGET.to_string(),
            })
        );
    }

    #[test]
    fn does_not_mount_worktree_for_shared_services() {
        let spec = build();
        let db = spec.service("db").expect("存在する");

        assert_eq!(
            db.source_mount, None,
            "共有サービスにどの worktree を見せるかは決められない"
        );
        assert!(db.key.workspace.is_shared());
        assert_eq!(
            db.attached_to.workspace, "feat-1",
            "共有でも呼び出し元のネットワークには繋ぐ"
        );
    }

    #[test]
    fn injects_context_env_vars() {
        let spec = build();
        let web = spec.service("web").expect("存在する");

        assert_eq!(
            web.env.get("MINATO_PROJECT").map(String::as_str),
            Some("myapp")
        );
        assert_eq!(
            web.env.get("MINATO_WORKSPACE").map(String::as_str),
            Some("feat-1")
        );
        assert_eq!(
            web.env.get("MINATO_SERVICE").map(String::as_str),
            Some("web")
        );
        assert_eq!(
            web.env.get("NODE_ENV").map(String::as_str),
            Some("development")
        );
    }

    #[test]
    fn user_env_overrides_injected_values() {
        let config = config(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            env = { MINATO_SERVICE = "custom" }
        "#,
        );

        let spec = build_workspace_spec(&config, "myapp", "feat-1", Path::new("/repo"))
            .expect("組み立てられる");

        assert_eq!(
            spec.services[0]
                .env
                .get("MINATO_SERVICE")
                .map(String::as_str),
            Some("custom"),
            "明示した値を勝手に潰さない"
        );
    }

    #[test]
    fn lists_peers_excluding_self() {
        let spec = build();
        let web = spec.service("web").expect("存在する");

        assert_eq!(web.peers, vec!["db".to_string()]);
        assert!(!web.peers.contains(&"web".to_string()), "自分は含めない");
    }

    #[test]
    fn parses_volumes_relative_to_worktree() {
        let spec = build();
        let db = spec.service("db").expect("存在する");

        assert_eq!(
            db.volumes,
            vec![VolumeMount::Named {
                name: "pgdata".into(),
                target: "/var/lib/postgresql/data".into(),
                read_only: false,
            }]
        );
    }

    #[test]
    fn rejects_build_with_actionable_message() {
        let config = config(
            r#"
            [project]
            name = "myapp"
            [services.web]
            build = "./web"
        "#,
        );

        let err = build_workspace_spec(&config, "myapp", "feat-1", Path::new("/repo")).unwrap_err();

        assert_eq!(err.code, minato_api::ErrorCode::Unsupported);
        assert!(err.message.contains("image"), "代わりの手段を示す: {err}");
    }

    #[test]
    fn rejects_unparseable_command() {
        let config = config(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            command = "sh -c 'unterminated"
        "#,
        );

        let err = build_workspace_spec(&config, "myapp", "feat-1", Path::new("/repo")).unwrap_err();
        assert_eq!(err.code, minato_api::ErrorCode::InvalidConfig);
    }
}
