//! Turning `minato.toml` into the spec a runtime is handed.
//!
//! A runtime knows nothing about `minato.toml`. Every question of
//! interpretation is settled here, and the runtime receives resolved
//! values only.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use minato_api::ApiError;
use minato_core::config::MOUNT_TARGET;
use minato_core::{MinatoConfig, ServiceConfig, ServiceScope};
use minato_runtime::{
    BuildSpec, ServiceSpec, SourceMount, VolumeMount, WorkspaceKey, WorkspaceSpec,
};

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

/// Works out what to build, and under what tag.
///
/// **The context comes from the worktree, not the main checkout.** A branch
/// that edits its Dockerfile has to get the image that Dockerfile describes;
/// building from the main worktree would hand it somebody else's.
///
/// The tag carries a fingerprint of what went into the image, which settles
/// both halves of the problem at once. Two worktrees whose Dockerfiles agree
/// land on the same tag and share one image, built once. A worktree that
/// changes anything lands on a different tag, so neither overwrites the
/// other, and "does this need building?" is just "does this tag exist?".
fn build_spec(
    service: &ServiceConfig,
    name: &str,
    project: &str,
    context: &str,
    worktree_path: &Path,
) -> Result<BuildSpec, ApiError> {
    let context = resolve_within(worktree_path, context, name, "build")?;

    let dockerfile = match &service.dockerfile {
        Some(path) => resolve_within(worktree_path, path, name, "dockerfile")?,
        None => context.join("Dockerfile"),
    };

    if !dockerfile.is_file() {
        return Err(ApiError::new(
            minato_api::ErrorCode::InvalidConfig,
            format!(
                "service `{name}`: no Dockerfile at {}",
                dockerfile.display()
            ),
        )
        .with_hint("point dockerfile at it, or add one to the build context"));
    }

    let fingerprint = fingerprint(&dockerfile, &service.build_args).map_err(|err| {
        ApiError::internal(format!(
            "service `{name}`: cannot read {}: {err}",
            dockerfile.display()
        ))
    })?;

    Ok(BuildSpec {
        context,
        dockerfile,
        tag: format!("minato-{project}-{name}:{fingerprint}"),
        fingerprint,
        args: service.build_args.clone(),
    })
}

/// Resolves a configured path, keeping it inside the worktree.
///
/// `build = "../../../etc"` would otherwise send the whole directory to the
/// runtime as a build context. The paths come from a committed file, so this
/// is about catching a mistake rather than an attack, but the failure mode
/// is bad enough to be worth refusing.
fn resolve_within(
    worktree_path: &Path,
    path: &str,
    service: &str,
    key: &str,
) -> Result<PathBuf, ApiError> {
    let joined = worktree_path.join(path);

    // `canonicalize` needs the path to exist, and a missing build context
    // deserves its own message rather than a confusing io error.
    if !joined.exists() {
        return Err(ApiError::new(
            minato_api::ErrorCode::InvalidConfig,
            format!(
                "service `{service}`: {key} points at {}, which does not exist",
                joined.display()
            ),
        ));
    }

    let resolved = joined.canonicalize().map_err(|err| {
        ApiError::internal(format!(
            "service `{service}`: cannot resolve {}: {err}",
            joined.display()
        ))
    })?;

    let root = worktree_path
        .canonicalize()
        .unwrap_or_else(|_| worktree_path.to_path_buf());
    if !resolved.starts_with(&root) {
        return Err(ApiError::new(
            minato_api::ErrorCode::InvalidConfig,
            format!(
                "service `{service}`: {key} points outside the worktree ({})",
                resolved.display()
            ),
        )
        .with_hint("a build context has to live in the repository"));
    }

    Ok(resolved)
}

/// What the image was built from, as a short hash.
///
/// Covers the Dockerfile and the build args. **A file the Dockerfile copies
/// in is not covered** — that would mean parsing the Dockerfile to find out
/// which files those are. So editing `package.json` does not by itself cause
/// a rebuild; `minato up --build` forces one. The same limitation applies to
/// `docker compose up`.
fn fingerprint(dockerfile: &Path, args: &BTreeMap<String, String>) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(std::fs::read(dockerfile)?);

    // BTreeMap iterates in order, so the same args always hash the same.
    for (key, value) in args {
        hasher.update(key.as_bytes());
        hasher.update(b"=");
        hasher.update(value.as_bytes());
        hasher.update(b"\0");
    }

    Ok(format!("{:x}", hasher.finalize())[..12].to_string())
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
    // Either a prebuilt image to pull, or a context to build. The
    // configuration has already rejected both and neither.
    let build = match &service.build {
        Some(context) => Some(build_spec(service, name, project, context, worktree_path)?),
        None => None,
    };

    let image = match (&service.image, &build) {
        (Some(image), _) => image.clone(),
        (None, Some(build)) => build.tag.clone(),
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
        build,
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

    /// A worktree with a Dockerfile in it, since building resolves real
    /// paths and refuses what is not there.
    fn worktree(dockerfile: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("web")).expect("creates");
        std::fs::write(dir.path().join("web/Dockerfile"), dockerfile).expect("writes");
        dir
    }

    fn built(dir: &Path, toml: &str) -> Result<WorkspaceSpec, ApiError> {
        build_workspace_spec(&config(toml), "myapp", "feat-1", dir, &no_envs())
    }

    const BUILDS: &str = r#"
        [project]
        name = "myapp"
        [services.web]
        build = "./web"
        port = 3000
    "#;

    #[test]
    fn a_built_service_gets_a_tag_of_its_own() {
        let dir = worktree("FROM scratch\n");
        let spec = built(dir.path(), BUILDS).expect("builds");
        let web = spec.service("web").expect("exists");

        let build = web.build.as_ref().expect("has a build");
        assert_eq!(web.image, build.tag, "the image to run is what gets built");
        assert!(
            build.tag.starts_with("minato-myapp-web:"),
            "got: {}",
            build.tag
        );
        assert_eq!(build.dockerfile, build.context.join("Dockerfile"));
    }

    #[test]
    fn the_same_dockerfile_gives_the_same_tag() {
        // Two worktrees that agree share one image rather than building it
        // twice, and that only works if the tag is a function of the input.
        let one = worktree("FROM scratch\n");
        let two = worktree("FROM scratch\n");

        let a = built(one.path(), BUILDS).expect("builds");
        let b = built(two.path(), BUILDS).expect("builds");

        assert_eq!(
            a.service("web").expect("exists").image,
            b.service("web").expect("exists").image
        );
    }

    #[test]
    fn editing_the_dockerfile_changes_the_tag() {
        // This is what makes a rebuild happen at all: the old tag stays
        // valid for whoever is still on the old Dockerfile.
        let one = worktree("FROM scratch\n");
        let two = worktree("FROM alpine\n");

        let a = built(one.path(), BUILDS).expect("builds");
        let b = built(two.path(), BUILDS).expect("builds");

        assert_ne!(
            a.service("web").expect("exists").image,
            b.service("web").expect("exists").image
        );
    }

    #[test]
    fn build_args_are_part_of_the_tag() {
        // Same Dockerfile, different args, different image. Sharing the tag
        // would hand one service the other's build.
        let dir = worktree("FROM scratch\n");

        let plain = built(dir.path(), BUILDS).expect("builds");
        let with_args = built(
            dir.path(),
            r#"
            [project]
            name = "myapp"
            [services.web]
            build = "./web"
            port = 3000
            build_args = { VERSION = "2" }
        "#,
        )
        .expect("builds");

        assert_ne!(
            plain.service("web").expect("exists").image,
            with_args.service("web").expect("exists").image
        );
    }

    #[test]
    fn a_missing_dockerfile_is_named_as_such() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("web")).expect("creates");

        let err = built(dir.path(), BUILDS).unwrap_err();

        assert_eq!(err.code, minato_api::ErrorCode::InvalidConfig);
        assert!(err.message.contains("Dockerfile"), "got: {err}");
    }

    #[test]
    fn a_missing_context_is_named_as_such() {
        // Distinct from a missing Dockerfile: the fix is different.
        let dir = tempfile::tempdir().expect("tempdir");

        let err = built(dir.path(), BUILDS).unwrap_err();

        assert!(err.message.contains("does not exist"), "got: {err}");
    }

    #[test]
    fn a_context_outside_the_worktree_is_refused() {
        // `build = "../.."` would hand the runtime a build context of
        // somebody's home directory.
        let dir = worktree("FROM scratch\n");

        let err = built(
            dir.path(),
            r#"
            [project]
            name = "myapp"
            [services.web]
            build = "../.."
            port = 3000
        "#,
        )
        .unwrap_err();

        assert!(err.message.contains("outside the worktree"), "got: {err}");
    }

    #[test]
    fn a_dockerfile_can_live_outside_the_context() {
        // One context, several images is a common layout.
        let dir = worktree("FROM scratch\n");
        std::fs::write(dir.path().join("Dockerfile.web"), "FROM alpine\n").expect("writes");

        let spec = built(
            dir.path(),
            r#"
            [project]
            name = "myapp"
            [services.web]
            build = "."
            dockerfile = "./Dockerfile.web"
            port = 3000
        "#,
        )
        .expect("builds");

        let build = spec
            .service("web")
            .expect("exists")
            .build
            .as_ref()
            .expect("has a build");

        assert!(
            build.dockerfile.ends_with("Dockerfile.web"),
            "got: {build:?}"
        );
    }

    #[test]
    fn a_prebuilt_image_has_no_build() {
        let spec = build();
        assert!(spec.service("web").expect("exists").build.is_none());
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
