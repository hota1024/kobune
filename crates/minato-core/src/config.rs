//! The schema and validation of `minato.toml`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::naming;

/// The name of the configuration file.
pub const CONFIG_FILE: &str = "minato.toml";

/// Where the worktree's source is mounted inside the container.
pub const MOUNT_TARGET: &str = "/workspace";

/// Where a service can write things worth keeping but not committing.
///
/// **Deliberately outside [`MOUNT_TARGET`].** Anywhere under the worktree
/// is the host's disk, inside the repository — which is how a package
/// store ends up as a gigabyte of untracked files in someone's checkout.
/// Handed to every service as `MINATO_CACHE_DIR`.
pub const CACHE_TARGET: &str = "/var/cache/minato";

/// The name of the volume behind [`CACHE_TARGET`].
///
/// **Deliberately not a valid volume name.** [`naming::is_valid_label`]
/// allows only lowercase letters, digits and hyphens, and `VolumeMount`
/// checks every declared name against it — so no `volumes` entry can reach
/// this one however it is spelled.
///
/// Calling it `cache` would have collided with anyone already using that
/// name: their storage would quietly have become the cache, which is the
/// sort of migration nobody notices until the data looks gone.
pub const CACHE_VOLUME: &str = "_cache";

/// The default when `idle_timeout` is omitted.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MinatoConfig {
    pub project: ProjectSection,

    #[serde(default)]
    pub runtime: RuntimeSection,

    #[serde(default)]
    pub services: IndexMap<String, ServiceConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSection {
    pub name: String,

    /// The URL suffix. Defaults to `{name}.localhost`.
    #[serde(default)]
    pub domain: Option<String>,

    /// Files to copy into a new worktree, relative to the repository root.
    ///
    /// For what git does not carry: an untracked but required `.env`.
    #[serde(default)]
    pub carry: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSection {
    #[serde(default = "default_runtime")]
    pub default: String,
}

impl Default for RuntimeSection {
    fn default() -> Self {
        Self {
            default: default_runtime(),
        }
    }
}

fn default_runtime() -> String {
    "docker".to_string()
}

/// Whether a `volumes` source asks for per-worktree storage.
///
/// **A host path is never scoped**, however it is spelled: there is nothing
/// to namespace about a directory the user already owns, and `@` is legal
/// in a path. Without this, a real directory called `certs@workspace` on a
/// shared service would be refused with a message describing something
/// nobody wrote.
///
/// Mirrors how `VolumeMount::parse` decides the same thing, in
/// `minato-runtime`. It cannot be called from here — the runtime sits above
/// this crate — so the one rule the two share is the prefix test, kept
/// deliberately trivial so it can be read side by side.
fn is_workspace_scoped(source: &str) -> bool {
    let is_host_path =
        source.starts_with('/') || source.starts_with('.') || source.starts_with('~');

    !is_host_path && source.ends_with("@workspace")
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    /// A prebuilt image. Mutually exclusive with `build`.
    #[serde(default)]
    pub image: Option<String>,

    /// The build context, relative to the repository root. Mutually
    /// exclusive with `image`.
    #[serde(default)]
    pub build: Option<String>,

    /// The Dockerfile, relative to the repository root.
    ///
    /// Defaults to `Dockerfile` inside the build context. Naming it
    /// separately is what lets several services build different images
    /// from one context.
    #[serde(default)]
    pub dockerfile: Option<String>,

    /// `--build-arg` values.
    ///
    /// A `BTreeMap` so the order is stable: these feed the fingerprint that
    /// decides whether a rebuild is needed, and a map that reordered itself
    /// would rebuild at random.
    #[serde(default)]
    pub build_args: BTreeMap<String, String>,

    /// The port the service listens on inside the container.
    #[serde(default)]
    pub port: Option<u16>,

    /// The start command. Falls back to the image's CMD.
    #[serde(default)]
    pub command: Option<String>,

    /// What to run once, before the service first starts.
    ///
    /// **Not once per container.** A stopped container is recreated by the
    /// next `up`, so tying this to container creation would run it on
    /// every `down`/`up` — which is the thing it exists to stop. It is
    /// remembered against the worktree, and runs again when this changes.
    #[serde(default)]
    pub setup: Option<String>,

    /// The working directory inside the container. Defaults to [`MOUNT_TARGET`].
    #[serde(default)]
    pub workdir: Option<String>,

    /// How readiness is determined. Used by scale-to-zero.
    #[serde(default)]
    pub health: Option<HealthCheck>,

    /// How long without traffic before the service is stopped.
    #[serde(default, with = "humantime_serde::option")]
    pub idle_timeout: Option<Duration>,

    #[serde(default)]
    pub env: BTreeMap<String, String>,

    #[serde(default)]
    pub depends_on: Vec<String>,

    #[serde(default)]
    pub scope: ServiceScope,

    /// Whether to publish a URL. Defaults to true when `port` is set.
    #[serde(default)]
    pub expose: Option<bool>,

    #[serde(default)]
    pub volumes: Vec<String>,
}

impl ServiceConfig {
    /// The effective value of `expose`.
    pub fn exposed(&self) -> bool {
        self.expose.unwrap_or(self.port.is_some())
    }

    pub fn workdir(&self) -> &str {
        self.workdir.as_deref().unwrap_or(MOUNT_TARGET)
    }

    pub fn idle_timeout(&self) -> Duration {
        self.idle_timeout.unwrap_or(DEFAULT_IDLE_TIMEOUT)
    }
}

/// Whether a service gets one instance per worktree or one per project.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceScope {
    /// One independent instance per worktree.
    #[default]
    Workspace,
    /// A single instance shared by every worktree of the project.
    Project,
}

/// How readiness is determined.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub enum HealthCheck {
    /// Ready when the response is 2xx or 3xx.
    Http(String),
    /// Ready when a TCP connection can be established.
    Tcp(String),
    /// Runs inside the container; ready on exit code 0.
    Cmd(String),
}

impl FromStr for HealthCheck {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let s = s.trim();
        if let Some(rest) = s.strip_prefix("cmd:") {
            let cmd = rest.trim();
            if cmd.is_empty() {
                return Err("nothing follows `cmd:`".to_string());
            }
            return Ok(Self::Cmd(cmd.to_string()));
        }
        if s.starts_with("http://") || s.starts_with("https://") {
            return Ok(Self::Http(s.to_string()));
        }
        if let Some(rest) = s.strip_prefix("tcp://") {
            if rest.is_empty() {
                return Err("nothing follows `tcp://`".to_string());
            }
            return Ok(Self::Tcp(rest.to_string()));
        }
        Err(format!(
            "cannot parse `{s}` as a health check. \
             Use `http://...`, `https://...`, `tcp://host:port` or `cmd:...`"
        ))
    }
}

impl TryFrom<String> for HealthCheck {
    type Error = String;

    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<HealthCheck> for String {
    fn from(value: HealthCheck) -> Self {
        match value {
            HealthCheck::Http(url) => url,
            HealthCheck::Tcp(addr) => format!("tcp://{addr}"),
            HealthCheck::Cmd(cmd) => format!("cmd:{cmd}"),
        }
    }
}

impl MinatoConfig {
    /// Searches upwards from `start` for `minato.toml`.
    ///
    /// Returns the path found along with the parsed configuration.
    pub fn find(start: &Path) -> Result<(PathBuf, Self)> {
        let mut dir = Some(start);
        while let Some(current) = dir {
            let candidate = current.join(CONFIG_FILE);
            if candidate.is_file() {
                let config = Self::load(&candidate)?;
                return Ok((candidate, config));
            }
            dir = current.parent();
        }
        Err(Error::ConfigNotFound(start.to_path_buf()))
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|source| Error::ConfigRead {
            path: path.to_path_buf(),
            source,
        })?;

        let config: Self = toml::from_str(&text).map_err(|source| Error::ConfigParse {
            path: path.to_path_buf(),
            source,
        })?;

        config.validate()?;
        Ok(config)
    }

    /// The URL suffix. `{name}.localhost` when `[project] domain` is unset.
    pub fn domain(&self) -> String {
        self.project
            .domain
            .clone()
            .unwrap_or_else(|| format!("{}.localhost", self.project.name))
    }

    pub fn service(&self, name: &str) -> Result<&ServiceConfig> {
        self.services
            .get(name)
            .ok_or_else(|| Error::ServiceNotFound(name.to_string()))
    }

    /// Checks that a syntactically valid configuration also makes sense.
    pub fn validate(&self) -> Result<()> {
        if !naming::is_valid_label(&self.project.name) {
            return Err(Error::ConfigInvalid(format!(
                "[project] name = \"{}\" is not a usable DNS label. \
                 Use lowercase letters, digits and hyphens, up to 63 characters",
                self.project.name
            )));
        }

        for entry in &self.project.carry {
            self.validate_carry_entry(entry)?;
        }

        if self.services.is_empty() {
            return Err(Error::ConfigInvalid("no services are defined".to_string()));
        }

        for (name, svc) in &self.services {
            self.validate_service(name, svc)?;
        }

        self.validate_no_dependency_cycle()?;
        Ok(())
    }

    /// Checks one `carry` entry before anything is copied.
    ///
    /// **These name files Minato reads on the user's behalf**, and a
    /// `minato.toml` arrives with a cloned repository as readily as it is
    /// written by hand. Anything reaching outside the repository is refused
    /// here rather than at copy time, so a bad entry is a configuration error
    /// with a clear message instead of a surprise during `minato new`.
    ///
    /// Syntax only. A symlink inside the repository can still point out of it,
    /// and that is caught where the copy happens, against the resolved path.
    fn validate_carry_entry(&self, entry: &str) -> Result<()> {
        let refuse = |why: &str| {
            Err(Error::ConfigInvalid(format!(
                "[project] carry entry `{entry}` {why}. Use a path relative to \
                 the repository root, like \".env\" or \"apps/api/.env\""
            )))
        };

        if entry.trim().is_empty() {
            return refuse("is empty");
        }

        let path = Path::new(entry);

        // Its own message: `~/x` is not an absolute path, and saying it is
        // sends someone looking for a leading slash they never wrote.
        if entry.starts_with('~') {
            return refuse("starts with ~, which Minato does not expand");
        }

        if path.is_absolute() {
            return refuse("is an absolute path");
        }

        if path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return refuse("leaves the repository");
        }

        Ok(())
    }

    fn validate_service(&self, name: &str, svc: &ServiceConfig) -> Result<()> {
        if !naming::is_valid_label(name) {
            return Err(Error::ConfigInvalid(format!(
                "the service name `{name}` is not a usable DNS label. \
                 Use lowercase letters, digits and hyphens, up to 63 characters"
            )));
        }

        match (&svc.image, &svc.build) {
            (Some(_), Some(_)) => {
                return Err(Error::ConfigInvalid(format!(
                    "service `{name}`: image and build are mutually exclusive"
                )));
            }
            (None, None) => {
                return Err(Error::ConfigInvalid(format!(
                    "service `{name}`: one of image or build is required"
                )));
            }
            _ => {}
        }

        if svc.dockerfile.is_some() && svc.build.is_none() {
            return Err(Error::ConfigInvalid(format!(
                "service `{name}`: dockerfile needs build to say which \
                 context to build in"
            )));
        }

        if !svc.build_args.is_empty() && svc.build.is_none() {
            return Err(Error::ConfigInvalid(format!(
                "service `{name}`: build_args has no effect without build"
            )));
        }

        if svc.expose == Some(true) && svc.port.is_none() {
            return Err(Error::ConfigInvalid(format!(
                "service `{name}`: expose = true requires a port"
            )));
        }

        for dep in &svc.depends_on {
            let target = self.services.get(dep).ok_or_else(|| {
                Error::ConfigInvalid(format!(
                    "service `{name}`: depends_on refers to undefined service `{dep}`"
                ))
            })?;

            // A shared instance depending on a per-worktree one leaves no
            // way to decide which worktree's instance to connect to.
            if svc.scope == ServiceScope::Project && target.scope == ServiceScope::Workspace {
                return Err(Error::ConfigInvalid(format!(
                    "service `{name}` (scope = \"project\") depends on \
                     `{dep}` (scope = \"workspace\"). A shared service cannot \
                     depend on a per-worktree one"
                )));
            }
        }

        // The name cannot be taken — `_cache` is not a valid volume name —
        // but the place it is mounted can be. Two mounts on one target is
        // an error from the container engine, several steps away from the
        // line that caused it.
        for volume in &svc.volumes {
            let mut parts = volume.split(':');
            let _source = parts.next();

            if parts.next() == Some(CACHE_TARGET) {
                return Err(Error::ConfigInvalid(format!(
                    "service `{name}`: {CACHE_TARGET} is where MINATO_CACHE_DIR \
                     is already mounted, so `{volume}` would be a second mount \
                     on the same path. Write under $MINATO_CACHE_DIR, or mount \
                     yours somewhere else"
                )));
            }
        }

        // Same reason, for storage rather than services: one instance
        // serves every worktree, so there is no worktree whose volume it
        // would be. Caught here rather than at start, where it would come
        // out as a container mounting whichever one it happened to make.
        if svc.scope == ServiceScope::Project {
            for volume in &svc.volumes {
                if volume.split(':').next().is_some_and(is_workspace_scoped) {
                    return Err(Error::ConfigInvalid(format!(
                        "service `{name}` (scope = \"project\") asks for the \
                         workspace-scoped volume `{volume}`. A shared service \
                         has no worktree to keep one per"
                    )));
                }
            }
        }

        Ok(())
    }

    fn validate_no_dependency_cycle(&self) -> Result<()> {
        #[derive(Clone, Copy, PartialEq)]
        enum Mark {
            Unvisited,
            InProgress,
            Done,
        }

        let mut marks: IndexMap<&str, Mark> = self
            .services
            .keys()
            .map(|k| (k.as_str(), Mark::Unvisited))
            .collect();

        // Explicit-stack DFS. The path is kept so the cycle can be named.
        for root in self.services.keys() {
            if marks[root.as_str()] != Mark::Unvisited {
                continue;
            }

            let mut path: Vec<&str> = vec![root.as_str()];
            let mut cursor: Vec<usize> = vec![0];
            marks[root.as_str()] = Mark::InProgress;

            while let Some(&node) = path.last() {
                let index = *cursor
                    .last()
                    .expect("cursor and path are pushed in lockstep");
                let deps = &self.services[node].depends_on;

                if index >= deps.len() {
                    marks[node] = Mark::Done;
                    path.pop();
                    cursor.pop();
                    continue;
                }

                *cursor.last_mut().expect("as above") += 1;
                let dep = deps[index].as_str();

                match marks[dep] {
                    Mark::Done => {}
                    Mark::InProgress => {
                        let start = path.iter().position(|n| *n == dep).unwrap_or(0);
                        let mut chain: Vec<&str> = path[start..].to_vec();
                        chain.push(dep);
                        return Err(Error::ConfigInvalid(format!(
                            "depends_on has a cycle: {}",
                            chain.join(" -> ")
                        )));
                    }
                    Mark::Unvisited => {
                        marks[dep] = Mark::InProgress;
                        path.push(dep);
                        cursor.push(0);
                    }
                }
            }
        }

        Ok(())
    }

    /// Orders service names so that dependencies come first.
    ///
    /// Every service is returned: [`Self::validate`] has already ruled out
    /// cycles.
    pub fn startup_order(&self) -> Vec<&str> {
        let mut ordered: Vec<&str> = Vec::with_capacity(self.services.len());
        let mut visited: IndexMap<&str, bool> =
            self.services.keys().map(|k| (k.as_str(), false)).collect();

        for root in self.services.keys() {
            let mut stack = vec![(root.as_str(), 0usize)];

            while let Some((node, index)) = stack.pop() {
                if visited[node] {
                    continue;
                }

                let deps = &self.services[node].depends_on;
                if index < deps.len() {
                    stack.push((node, index + 1));
                    let dep = deps[index].as_str();
                    if !visited[dep] {
                        stack.push((dep, 0));
                    }
                } else {
                    visited[node] = true;
                    ordered.push(node);
                }
            }
        }

        ordered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<MinatoConfig> {
        let config: MinatoConfig = toml::from_str(text).expect("syntax is assumed valid");
        config.validate()?;
        Ok(config)
    }

    const MINIMAL: &str = r#"
        [project]
        name = "myapp"

        [services.web]
        image = "node:22"
        port = 3000
    "#;

    #[test]
    fn parses_minimal_config() {
        let config = parse(MINIMAL).expect("valid");
        assert_eq!(config.project.name, "myapp");
        assert_eq!(config.runtime.default, "docker");
        assert_eq!(config.domain(), "myapp.localhost");

        let web = config.service("web").expect("exists");
        assert!(web.exposed());
        assert_eq!(web.workdir(), MOUNT_TARGET);
        assert_eq!(web.idle_timeout(), DEFAULT_IDLE_TIMEOUT);
    }

    #[test]
    fn parses_full_config() {
        let config = parse(
            r#"
            [project]
            name = "myapp"
            domain = "dev.example.com"

            [runtime]
            default = "docker"

            [services.web]
            image = "node:22"
            port = 3000
            command = "pnpm dev"
            health = "http://localhost:3000/healthz"
            idle_timeout = "15m"
            depends_on = ["db"]
            env = { NODE_ENV = "development" }

            [services.db]
            image = "postgres:16"
            port = 5432
            scope = "project"
            expose = false
            volumes = ["pgdata:/var/lib/postgresql/data"]
            health = "tcp://localhost:5432"
        "#,
        )
        .expect("valid");

        assert_eq!(config.domain(), "dev.example.com");

        let web = config.service("web").expect("exists");
        assert_eq!(web.idle_timeout(), Duration::from_secs(15 * 60));
        assert_eq!(
            web.health,
            Some(HealthCheck::Http("http://localhost:3000/healthz".into()))
        );

        let db = config.service("db").expect("exists");
        assert_eq!(db.scope, ServiceScope::Project);
        assert!(
            !db.exposed(),
            "expose = false hides the service even with a port"
        );
        assert_eq!(db.health, Some(HealthCheck::Tcp("localhost:5432".into())));
    }

    #[test]
    fn rejects_image_and_build_together() {
        let err = parse(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            build = "./web"
        "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("mutually exclusive"));
    }

    #[test]
    fn rejects_service_without_source() {
        let err = parse(
            r#"
            [project]
            name = "myapp"
            [services.web]
            port = 3000
        "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("one of image or build"));
    }

    #[test]
    fn accepts_files_to_carry() {
        let config = parse(
            r#"
            [project]
            name = "myapp"
            carry = [".env", "apps/api/.dev.vars"]
            [services.web]
            image = "node:22"
        "#,
        )
        .expect("is valid");

        assert_eq!(config.project.carry, vec![".env", "apps/api/.dev.vars"]);
    }

    #[test]
    fn carry_defaults_to_nothing() {
        let config = parse(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
        "#,
        )
        .expect("is valid");

        assert!(config.project.carry.is_empty());
    }

    #[test]
    fn refuses_carry_entries_that_leave_the_repository() {
        // These name files Minato reads on someone's behalf, and a
        // minato.toml arrives with a clone as readily as it is hand-written.
        // Asserted per entry: a message that is true of one of these and
        // not the others is exactly the drift worth catching.
        let cases = [
            ("../.env", "leaves the repository"),
            ("a/../../b", "leaves the repository"),
            ("/etc/passwd", "is an absolute path"),
            ("~/.aws/credentials", "which Minato does not expand"),
        ];

        for (entry, expected) in cases {
            let err = parse(&format!(
                r#"
                [project]
                name = "myapp"
                carry = ["{entry}"]
                [services.web]
                image = "node:22"
            "#
            ))
            .unwrap_err();

            let message = err.to_string();
            assert!(message.contains("carry"), "{entry}: {message}");
            assert!(message.contains(expected), "{entry}: {message}");
        }
    }

    #[test]
    fn refuses_an_empty_carry_entry() {
        let err = parse(
            r#"
            [project]
            name = "myapp"
            carry = ["  "]
            [services.web]
            image = "node:22"
        "#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("is empty"), "{err}");
    }

    #[test]
    fn rejects_expose_without_port() {
        let err = parse(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            expose = true
        "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("requires a port"));
    }

    #[test]
    fn rejects_unknown_dependency() {
        let err = parse(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            depends_on = ["nope"]
        "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("undefined service"));
    }

    #[test]
    fn rejects_shared_service_depending_on_workspace_service() {
        let err = parse(
            r#"
            [project]
            name = "myapp"
            [services.cache]
            image = "redis:7"
            scope = "project"
            depends_on = ["web"]
            [services.web]
            image = "node:22"
        "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("A shared service cannot"));
    }

    #[test]
    fn refuses_a_workspace_volume_on_a_shared_service() {
        // One instance serves every worktree, so there is no worktree whose
        // volume it would be.
        let err = parse(
            r#"
            [project]
            name = "myapp"
            [services.db]
            image = "postgres:16"
            scope = "project"
            volumes = ["pgdata@workspace:/var/lib/postgresql/data"]
        "#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("no worktree"), "{err}");
    }

    #[test]
    fn a_host_path_ending_in_workspace_is_not_a_scope() {
        // A real directory named `certs@workspace`. Refusing this would
        // fail the whole project with a message about something the user
        // did not write.
        parse(
            r#"
            [project]
            name = "myapp"
            [services.db]
            image = "postgres:16"
            scope = "project"
            volumes = ["./certs@workspace:/certs:ro"]
        "#,
        )
        .expect("a host path is never scoped, however it is spelled");
    }

    #[test]
    fn the_cache_volume_cannot_be_named() {
        // Not a reservation to remember — `_cache` is not a valid volume
        // name, so no spelling of `volumes` can reach it. Anyone already
        // using a volume called `cache` keeps it, and keeps its contents.
        assert!(!naming::is_valid_label(CACHE_VOLUME));

        parse(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            volumes = ["cache:/tmp/cache"]
        "#,
        )
        .expect("`cache` is an ordinary name, and stays one");
    }

    #[test]
    fn a_second_mount_on_the_cache_path_is_refused() {
        // Two mounts on one target is an error from the container engine,
        // a long way from the line that caused it.
        let err = parse(&format!(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            volumes = ["mine:{CACHE_TARGET}"]
        "#
        ))
        .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("MINATO_CACHE_DIR"), "{message}");
        assert!(!message.contains("  "), "run-together spacing: {message}");
    }

    #[test]
    fn mounting_below_the_cache_path_is_allowed() {
        parse(&format!(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            volumes = ["mine:{CACHE_TARGET}/mine"]
        "#
        ))
        .expect("only the exact path is taken");
    }

    #[test]
    fn a_workspace_volume_is_fine_on_a_per_worktree_service() {
        parse(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            volumes = ["node-modules@workspace:/workspace/node_modules"]
        "#,
        )
        .expect("this is the case the scope exists for");
    }

    #[test]
    fn rejects_dependency_cycle() {
        let err = parse(
            r#"
            [project]
            name = "myapp"
            [services.a]
            image = "x"
            depends_on = ["b"]
            [services.b]
            image = "x"
            depends_on = ["a"]
        "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("has a cycle"), "got: {err}");
    }

    #[test]
    fn rejects_self_dependency() {
        let err = parse(
            r#"
            [project]
            name = "myapp"
            [services.a]
            image = "x"
            depends_on = ["a"]
        "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("has a cycle"), "got: {err}");
    }

    #[test]
    fn rejects_invalid_project_name() {
        let err = parse(
            r#"
            [project]
            name = "My App"
            [services.web]
            image = "node:22"
        "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("not a usable DNS label"));
    }

    #[test]
    fn rejects_unknown_field() {
        let result: std::result::Result<MinatoConfig, _> = toml::from_str(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            porrt = 3000
        "#,
        );
        assert!(result.is_err(), "a typo must be caught");
    }

    #[test]
    fn startup_order_respects_dependencies() {
        let config = parse(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "x"
            depends_on = ["api"]
            [services.api]
            image = "x"
            depends_on = ["db"]
            [services.db]
            image = "x"
        "#,
        )
        .expect("valid");

        let order = config.startup_order();
        assert_eq!(order.len(), 3);

        let pos = |name: &str| order.iter().position(|s| *s == name).expect("present");
        assert!(pos("db") < pos("api"));
        assert!(pos("api") < pos("web"));
    }

    #[test]
    fn startup_order_handles_diamond() {
        let config = parse(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "x"
            depends_on = ["api", "worker"]
            [services.api]
            image = "x"
            depends_on = ["db"]
            [services.worker]
            image = "x"
            depends_on = ["db"]
            [services.db]
            image = "x"
        "#,
        )
        .expect("valid");

        let order = config.startup_order();
        assert_eq!(
            order.len(),
            4,
            "every service appears exactly once: {order:?}"
        );

        let pos = |name: &str| order.iter().position(|s| *s == name).expect("present");
        assert!(pos("db") < pos("api"));
        assert!(pos("db") < pos("worker"));
        assert!(pos("api") < pos("web"));
        assert!(pos("worker") < pos("web"));
    }

    #[test]
    fn health_check_roundtrip() {
        let cases = [
            (
                "http://localhost:3000/healthz",
                "http://localhost:3000/healthz",
            ),
            ("tcp://localhost:5432", "tcp://localhost:5432"),
            ("cmd:pg_isready", "cmd:pg_isready"),
        ];

        for (input, expected) in cases {
            let parsed: HealthCheck = input.parse().expect("parses");
            let back: String = parsed.into();
            assert_eq!(back, expected);
        }
    }

    #[test]
    fn health_check_rejects_unknown_scheme() {
        assert!("ftp://x".parse::<HealthCheck>().is_err());
        assert!("localhost:3000".parse::<HealthCheck>().is_err());
    }
}
