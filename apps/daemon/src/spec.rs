//! Turning `minato.toml` into the spec a runtime is handed.
//!
//! A runtime knows nothing about `minato.toml`. Every question of
//! interpretation is settled here, and the runtime receives resolved
//! values only.

use std::collections::BTreeMap;
use std::path::Path;

use minato_api::ApiError;
use minato_core::config::MOUNT_TARGET;
use minato_core::{MinatoConfig, ServiceConfig, ServiceScope};
use minato_runtime::{ServiceSpec, SourceMount, VolumeMount, WorkspaceKey, WorkspaceSpec};

/// Builds the spec for a whole workspace.
pub fn build_workspace_spec(
    config: &MinatoConfig,
    project: &str,
    workspace: &str,
    worktree_path: &Path,
    envs: &BTreeMap<String, BTreeMap<String, String>>,
) -> Result<WorkspaceSpec, ApiError> {
    let key = WorkspaceKey::new(project, workspace);

    // Ordered so dependencies come first. The runtime starts them in
    // this order.
    let ordered = config.startup_order();
    let mut services = Vec::with_capacity(ordered.len());

    for name in ordered {
        let service_config = config
            .services
            .get(name)
            .expect("startup_order only returns services that exist");

        services.push(build_service_spec(
            service_config,
            name,
            project,
            workspace,
            worktree_path,
            envs.get(name).cloned().unwrap_or_default(),
            config.services.keys().cloned().collect(),
        )?);
    }

    Ok(WorkspaceSpec {
        key,
        worktree_path: worktree_path.to_path_buf(),
        services,
    })
}

/// Builds the spec for one service.
pub fn build_service_spec(
    service: &ServiceConfig,
    name: &str,
    project: &str,
    workspace: &str,
    worktree_path: &Path,
    env: BTreeMap<String, String>,
    all_services: Vec<String>,
) -> Result<ServiceSpec, ApiError> {
    // Prebuilt images only for now. Building a Dockerfile comes in M0.5.
    let image = match (&service.image, &service.build) {
        (Some(image), _) => image.clone(),
        (None, Some(_)) => {
            return Err(ApiError::unsupported(format!(
                "service `{name}`: building an image with build is not \
                 supported yet. Name a prebuilt image with image"
            )));
        }
        (None, None) => {
            return Err(ApiError::unsupported(format!(
                "service `{name}`: no image was given"
            )));
        }
    };

    let command = match &service.command {
        Some(raw) => Some(shell_words::split(raw).map_err(|err| {
            ApiError::new(
                minato_api::ErrorCode::InvalidConfig,
                format!("service `{name}`: cannot make sense of command: {err}"),
            )
        })?),
        None => None,
    };

    let attached_to = WorkspaceKey::new(project, workspace);

    // A shared service belongs to no particular worktree.
    let key = match service.scope {
        ServiceScope::Workspace => attached_to.service(name),
        ServiceScope::Project => WorkspaceKey::shared(project).service(name),
    };

    // Mounting a worktree's source into a shared service leaves no answer
    // to which worktree it should be showing.
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
                format!("service `{name}`: {message}"),
            )
        })?);
    }

    // The other services in this workspace. Resolving their names is the
    // runtime's job.
    let peers: Vec<String> = all_services
        .into_iter()
        .filter(|other| other != name)
        .collect();

    Ok(ServiceSpec {
        key,
        attached_to,
        image,
        command,
        workdir: service.workdir().to_string(),
        env,
        port: service.port,
        health: service.health.clone(),
        scope: service.scope,
        volumes,
        source_mount,
        peers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn config(toml: &str) -> MinatoConfig {
        let config: MinatoConfig = toml::from_str(toml).expect("is syntactically valid");
        config.validate().expect("is semantically valid");
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

    /// The environment layers are beside the point here, so they go in
    /// empty. Stacking them is `crate::env`'s job.
    fn no_envs() -> BTreeMap<String, BTreeMap<String, String>> {
        BTreeMap::new()
    }

    fn build() -> WorkspaceSpec {
        build_workspace_spec(
            &config(SAMPLE),
            "myapp",
            "feat-1",
            Path::new("/repo/wt/feat-1"),
            &no_envs(),
        )
        .expect("builds")
    }

    #[test]
    fn orders_services_by_dependency() {
        let spec = build();
        let names: Vec<&str> = spec.services.iter().map(|s| s.name()).collect();
        assert_eq!(names, vec!["db", "web"], "dependencies come first");
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

        let spec = build_workspace_spec(&config, "myapp", "feat-1", Path::new("/repo"), &no_envs())
            .expect("builds");

        assert_eq!(
            spec.services[0].command,
            Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo hello world".to_string()
            ]),
            "what is quoted stays one argument"
        );
    }

    #[test]
    fn mounts_worktree_for_workspace_scoped_services() {
        let spec = build();
        let web = spec.service("web").expect("exists");

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
        let db = spec.service("db").expect("exists");

        assert_eq!(
            db.source_mount, None,
            "there is no answer to which worktree a shared service sees"
        );
        assert!(db.key.workspace.is_shared());
        assert_eq!(
            db.attached_to.workspace, "feat-1",
            "shared or not, it joins the caller's network"
        );
    }

    #[test]
    fn lists_peers_excluding_self() {
        let spec = build();
        let web = spec.service("web").expect("exists");

        assert_eq!(web.peers, vec!["db".to_string()]);
        assert!(
            !web.peers.contains(&"web".to_string()),
            "it is not its own peer"
        );
    }

    #[test]
    fn parses_volumes_relative_to_worktree() {
        let spec = build();
        let db = spec.service("db").expect("exists");

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

        let err = build_workspace_spec(&config, "myapp", "feat-1", Path::new("/repo"), &no_envs())
            .unwrap_err();

        assert_eq!(err.code, minato_api::ErrorCode::Unsupported);
        assert!(
            err.message.contains("image"),
            "it points at what to do instead: {err}"
        );
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

        let err = build_workspace_spec(&config, "myapp", "feat-1", Path::new("/repo"), &no_envs())
            .unwrap_err();
        assert_eq!(err.code, minato_api::ErrorCode::InvalidConfig);
    }
}
