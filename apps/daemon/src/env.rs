//! Building the environment a service receives.
//!
//! The layers stack as `docs/DESIGN.md` §8 describes. What Minato injects
//! goes first, so the user's own settings win. The other way round,
//! Minato's conveniences would quietly erase them.

use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use minato_api::{ApiError, ErrorCode};
use minato_core::{EnvLayers, EnvScope, MinatoConfig, Paths, WorkspaceRecord, env};

use crate::gateway::Gateway;

/// The variables Minato injects.
///
/// **`MINATO_URL_<SERVICE>` is the important one.** Without a way for the
/// frontend to learn the API's URL, a setup where URLs differ per worktree
/// cannot hold together.
///
/// `service` is `None` when nobody is asking about a particular one — a
/// listing of what every service shares. `MINATO_SERVICE` is left out
/// there, since there is no service to name.
pub fn injected(
    config: &MinatoConfig,
    project: &str,
    record: &WorkspaceRecord,
    service: Option<&str>,
    gateway: &Gateway,
) -> IndexMap<String, String> {
    let mut values = IndexMap::new();

    values.insert("MINATO_PROJECT".to_string(), project.to_string());
    values.insert("MINATO_WORKSPACE".to_string(), record.label.clone());

    if let Some(service) = service {
        values.insert("MINATO_SERVICE".to_string(), service.to_string());
    }

    // Somewhere to put what is worth keeping but not committing. Every
    // service is mounted a volume here, so a tool pointed at it stops
    // writing gigabytes into the worktree — and therefore into the
    // repository on the host.
    values.insert(
        "MINATO_CACHE_DIR".to_string(),
        minato_core::config::CACHE_TARGET.to_string(),
    );

    let domain = config.domain();
    for (name, service_config) in &config.services {
        if !service_config.exposed() {
            continue;
        }

        let host = minato_core::naming::service_host_in(name, record.url_label(), &domain);
        let Some(url) = gateway.url_for(&host) else {
            // With no proxy running there is no URL. An empty string
            // would leave it "set, but broken".
            continue;
        };

        values.insert(url_variable(name), url);

        // **A CORS origin, `allowedDevOrigins` and a cookie domain want
        // the host, not the URL.** Cutting the scheme off `MINATO_URL_*`
        // with `sed` is what every project does otherwise, and it is a
        // name Minato has already worked out to build the URL.
        //
        // Under the same condition as the URL on purpose: a hostname
        // nothing answers on is the "set, but broken" this avoids.
        values.insert(hostname_variable(name), host);
    }

    values
}

/// Turns a failure to settle the layers into an API error.
///
/// **A missing `MINATO_URL_<SERVICE>` is usually the proxy being down**,
/// not a mistake in the configuration: the variable is only injected while
/// the gateway is listening, and only for an exposed service. Saying no
/// more than "nothing sets it" sends someone to edit a `minato.toml` that
/// is already right. `MINATO_HOSTNAME_<SERVICE>` goes the same way, for
/// the same reason.
pub fn resolution_error(err: env::EnvError) -> ApiError {
    let error = ApiError::new(ErrorCode::InvalidConfig, err.to_string());

    match &err {
        env::EnvError::UndefinedReference { name, .. } if is_per_service(name) => error.with_hint(
            "MINATO_URL_<SERVICE> and MINATO_HOSTNAME_<SERVICE> exist only while the \
             proxy is listening, and only for a service with `expose = true`. Run \
             `minato doctor`",
        ),
        _ => error,
    }
}

/// Whether a name is one Minato injects per service.
fn is_per_service(name: &str) -> bool {
    name.starts_with("MINATO_URL_") || name.starts_with("MINATO_HOSTNAME_")
}

/// Why a listing is showing values as written.
///
/// **A listing degrades rather than failing**, so this says what went
/// wrong where an error would have. `service` is the one being listed, if
/// any: a listing of no particular service is missing things on purpose,
/// and what is missing on purpose is not something to go and fix.
pub fn listing_note(err: &env::EnvError, service: Option<&str>, config: &MinatoConfig) -> String {
    let note = format!("{err}. Values are shown as written");

    let env::EnvError::UndefinedReference { name, .. } = err else {
        return note;
    };

    // **A listing of no particular service leaves out `MINATO_SERVICE`
    // and every service's own `env`**, since presenting one service's
    // variables as everyone's would be worse. A value referring to one is
    // right, and starting the service settles it — it is the listing that
    // cannot, and "nothing sets it" would send someone hunting a bug that
    // is not there.
    if service.is_none() && only_a_service_has(name, config) {
        return format!(
            "{note}. This listing names no service, so {name} is not part of it — \
             `minato env ls --service <name>` settles it"
        );
    }

    if is_per_service(name) {
        return format!(
            "{note}. {name} exists only while the proxy is listening, and only for a \
             service with `expose = true`. Run `minato doctor`"
        );
    }

    note
}

/// Whether `name` is something only a listing about one service holds.
fn only_a_service_has(name: &str, config: &MinatoConfig) -> bool {
    name == "MINATO_SERVICE"
        || config
            .services
            .values()
            .any(|service| service.env.contains_key(name))
}

/// Turns a service name into a variable name.
///
/// `cache-store` becomes `MINATO_URL_CACHE_STORE`. A hyphen is not valid
/// in a variable name, so it becomes an underscore.
pub fn url_variable(service: &str) -> String {
    format!("MINATO_URL_{}", service.to_uppercase().replace('-', "_"))
}

/// The name carrying a service's hostname.
///
/// `MINATO_HOSTNAME_<SERVICE>`, and not `MINATO_HOST_`: that one is taken
/// by Apple Container, where it carries a peer's IP address. Two names a
/// letter apart meaning different things is worse than a longer one.
pub fn hostname_variable(service: &str) -> String {
    format!(
        "MINATO_HOSTNAME_{}",
        service.to_uppercase().replace('-', "_")
    )
}

/// Writes a service's settled environment into its worktree.
///
/// **For the tools that read a file rather than their process's
/// environment.** `wrangler dev` does not pass its own environment to the
/// Worker, and Vite reads `.env.local` off disk; without this, a project
/// writes a start-up script that turns variables back into a file.
///
/// Returns the path written, or `None` when it already held exactly this.
/// **Rewriting it unchanged would be a change to anything watching it** —
/// a dev server restarting itself every time scale-to-zero wakes the
/// service.
pub fn write_env_file(
    worktree: &Path,
    relative: &str,
    contents: &str,
) -> Result<Option<PathBuf>, ApiError> {
    let refuse = |why: &str| {
        Err(ApiError::new(
            ErrorCode::InvalidConfig,
            format!("env_file `{relative}`: {why}"),
        ))
    };

    // A generated file that git watches leaves the worktree permanently
    // dirty, and committing it would put one branch's URLs into every
    // other checkout.
    if minato_core::git::is_tracked(worktree, relative) {
        return refuse(
            "git tracks it. Point it somewhere untracked, `.minato/` or a \
             gitignored path",
        );
    }

    let path = worktree.join(relative);

    // Containment is checked against the deepest directory that already
    // exists, and therefore *before* anything is created: a symlinked
    // component would otherwise have directories made outside the worktree
    // on the way to finding out.
    let anchor = existing_ancestor(&path);
    let root = worktree.canonicalize().unwrap_or_else(|_| worktree.into());

    match anchor.canonicalize() {
        Ok(resolved) if !resolved.starts_with(&root) => {
            return refuse("resolves outside the worktree");
        }
        Ok(_) => {}
        Err(source) => {
            return Err(ApiError::internal(format!(
                "env_file `{relative}`: cannot resolve {}: {source}",
                anchor.display()
            )));
        }
    }

    let occupied = "there is already a file there that Minato did not write. \
                    Move it aside, or point env_file somewhere else";

    match std::fs::read_to_string(&path) {
        // Somebody's own file. Whatever it is for, it is not this.
        Ok(existing) if !env::is_generated(&existing) => return refuse(occupied),
        Ok(existing) if existing == contents => return Ok(None),
        Ok(_) => {}
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        // **Unreadable is not the same as absent.** A file in some other
        // encoding, or one this user cannot read, is still somebody's —
        // and the marker cannot say otherwise, so it gets left alone.
        Err(_) => return refuse(occupied),
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| {
            ApiError::internal(format!(
                "env_file `{relative}`: cannot create {}: {source}",
                parent.display()
            ))
        })?;
    }

    sweep_stale_temporaries(&path);

    // Written beside the target and renamed over it, so a service reading
    // the file never sees half of one.
    let temporary = temporary_beside(&path);
    env::write_file(&temporary, contents).map_err(|err| {
        let _ = std::fs::remove_file(&temporary);
        ApiError::internal(format!("env_file `{relative}`: {err}"))
    })?;

    std::fs::rename(&temporary, &path).map_err(|source| {
        let _ = std::fs::remove_file(&temporary);
        ApiError::internal(format!(
            "env_file `{relative}`: cannot write {}: {source}",
            path.display()
        ))
    })?;

    Ok(Some(path))
}

/// The nearest ancestor of `path` that exists.
///
/// Falls back to the path itself, which cannot escape anything: a
/// non-existent root has nothing above it either.
fn existing_ancestor(path: &Path) -> PathBuf {
    let mut candidate = path.parent();

    while let Some(dir) = candidate {
        if dir.exists() {
            return dir.to_path_buf();
        }
        candidate = dir.parent();
    }

    path.to_path_buf()
}

/// A sibling to write before renaming over the target.
///
/// A sibling rather than `/tmp`, because a rename across filesystems is
/// not atomic — and on macOS `/tmp` regularly is one.
///
/// **A fresh name every time.** A fixed one is a file an earlier crash can
/// leave behind, and the write that follows opens it rather than creating
/// it: a leftover symlink would be written straight through, out of the
/// worktree the containment check just held it inside, and a leftover
/// regular file would keep whatever permissions it had rather than 0600.
/// Two services writing at once would also rename each other's away.
fn temporary_beside(path: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or(0);
    let count = NEXT.fetch_add(1, Ordering::Relaxed);

    let directory = path.parent().unwrap_or(Path::new("."));
    directory.join(format!(
        "{}{}-{nonce}-{count}{TEMPORARY_SUFFIX}",
        temporary_prefix(path),
        std::process::id()
    ))
}

/// What every temporary for `path` starts with.
fn temporary_prefix(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "env".to_string());

    format!(".{name}.")
}

/// What every temporary ends with.
const TEMPORARY_SUFFIX: &str = ".minato-tmp";

/// Removes temporaries an earlier run left behind.
///
/// **Both failure paths clean up after themselves; a daemon that is killed
/// between the write and the rename cannot.** With a fresh name each time,
/// what it leaves never gets reused — it just accumulates in the worktree,
/// one file per crash, next to a file people do look at.
///
/// Only what some *other* process wrote: one of this daemon's own may
/// belong to a write happening right now, and deleting it would leave that
/// write renaming a file that is no longer there.
///
/// Best effort throughout. Failing a service's start over a leftover would
/// be worse than the leftover.
fn sweep_stale_temporaries(path: &Path) {
    let Some(directory) = path.parent() else {
        return;
    };

    let prefix = temporary_prefix(path);
    let mine = format!("{}{}-", prefix, std::process::id());

    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if !name.starts_with(&prefix) || !name.ends_with(TEMPORARY_SUFFIX) {
            continue;
        }

        if name.starts_with(&mine) {
            continue;
        }

        if std::fs::remove_file(entry.path()).is_ok() {
            tracing::debug!("removed a stale {}", entry.path().display());
        }
    }
}

/// Stacks one service's layers, lowest priority first.
pub fn layers_for_service(
    config: &MinatoConfig,
    project: &str,
    record: &WorkspaceRecord,
    project_root: &std::path::Path,
    service: Option<&str>,
    paths: &Paths,
    gateway: &Gateway,
) -> Result<EnvLayers, env::EnvError> {
    let mut layers = EnvLayers::new();

    // 1. What Minato injects — first, so the user can override it
    layers.push(
        EnvScope::Injected,
        injected(config, project, record, service, gateway),
    );

    // 2. global
    layers.push_file(EnvScope::Global, &paths.root().join(env::GLOBAL_ENV_FILE))?;

    // 3. The project-wide file
    layers.push_file(EnvScope::Project, &env::project_env_path(project_root))?;

    // 4. The service's own entry in minato.toml — more specific than the
    //    project.
    //
    // **Only when one was asked about.** Folding some service's own
    // variables into a listing of what every service shares would show
    // them as everyone's.
    //
    // The two conditions are kept apart on purpose: `None` means nobody
    // named a service, while a name that does not resolve is a caller's
    // mistake, and collapsing them would answer that mistake with a
    // plausible listing missing every one of the service's variables.
    if let Some(name) = service
        && let Ok(service_config) = config.service(name)
    {
        let values: IndexMap<String, String> = service_config
            .env
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        layers.push(EnvScope::Service, values);
    }

    // 5. The workspace — the most specific, so it goes last
    layers.push_file(EnvScope::Workspace, &env::workspace_env_path(&record.path))?;

    Ok(layers)
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
        [services.api-server]
        image = "node:22"
        port = 8080
        [services.db]
        image = "postgres:16"
        port = 5432
        expose = false
    "#;

    fn record(label: &str, is_main: bool) -> WorkspaceRecord {
        WorkspaceRecord {
            label: label.to_string(),
            branch: "feature/one".to_string(),
            path: PathBuf::from("/repo/wt/feat-1"),
            is_main,
            created_at: chrono::Utc::now(),
            setup_done: Default::default(),
        }
    }

    #[test]
    fn injects_urls_for_every_exposed_service() {
        // Without this the frontend has no way to learn the API's URL.
        let values = injected(
            &config(SAMPLE),
            "myapp",
            &record("feat-1", false),
            Some("web"),
            &Gateway::with_ports(Some(80), Some(443)),
        );

        assert_eq!(
            values.get("MINATO_URL_WEB").map(String::as_str),
            Some("https://web.feat-1.myapp.localhost")
        );
        assert_eq!(
            values.get("MINATO_URL_API_SERVER").map(String::as_str),
            Some("https://api-server.feat-1.myapp.localhost"),
            "a hyphen becomes an underscore"
        );
        assert!(
            !values.contains_key("MINATO_URL_DB"),
            "a service with expose = false has no URL"
        );
    }

    #[test]
    fn injects_the_service_own_identity() {
        let values = injected(
            &config(SAMPLE),
            "myapp",
            &record("feat-1", false),
            Some("web"),
            &Gateway::inert(),
        );

        assert_eq!(
            values.get("MINATO_PROJECT").map(String::as_str),
            Some("myapp")
        );
        assert_eq!(
            values.get("MINATO_WORKSPACE").map(String::as_str),
            Some("feat-1")
        );
        assert_eq!(
            values.get("MINATO_SERVICE").map(String::as_str),
            Some("web")
        );
    }

    #[test]
    fn every_service_is_told_where_it_may_write() {
        // Without this a tool that caches by default writes under
        // /workspace, which is the host's repository.
        let values = injected(
            &config(SAMPLE),
            "myapp",
            &record("feat-1", false),
            Some("web"),
            &Gateway::inert(),
        );

        let cache = values.get("MINATO_CACHE_DIR").map(String::as_str);
        assert_eq!(cache, Some(minato_core::config::CACHE_TARGET));
        assert!(
            !cache
                .expect("set")
                .starts_with(minato_core::config::MOUNT_TARGET),
            "pointing it into the worktree would defeat the purpose"
        );
    }

    #[test]
    fn omits_urls_when_the_proxy_is_down() {
        // An empty string would leave it "set, but unreachable".
        let values = injected(
            &config(SAMPLE),
            "myapp",
            &record("feat-1", false),
            Some("web"),
            &Gateway::inert(),
        );

        assert!(!values.keys().any(|key| key.starts_with("MINATO_URL_")));
        assert!(
            !values.keys().any(|key| key.starts_with("MINATO_HOSTNAME_")),
            "a hostname nothing answers on is the same trap"
        );
    }

    #[test]
    fn injects_the_hostname_beside_the_url() {
        // A CORS origin, `allowedDevOrigins` and a cookie domain want the
        // host on its own. Cutting the scheme off the URL with `sed` is
        // what every project does without this.
        let values = injected(
            &config(SAMPLE),
            "myapp",
            &record("feat-1", false),
            Some("web"),
            &Gateway::with_ports(Some(80), Some(443)),
        );

        assert_eq!(
            values.get("MINATO_HOSTNAME_WEB").map(String::as_str),
            Some("web.feat-1.myapp.localhost"),
            "no scheme, no port, no trailing slash"
        );
        assert_eq!(
            values.get("MINATO_HOSTNAME_API_SERVER").map(String::as_str),
            Some("api-server.feat-1.myapp.localhost"),
            "a hyphen becomes an underscore in the name, not in the host"
        );
        assert!(
            !values.contains_key("MINATO_HOSTNAME_DB"),
            "a service with expose = false publishes no hostname"
        );
    }

    #[test]
    fn the_hostname_holds_no_port_even_when_the_url_does() {
        // The port belongs to the URL. Anything asking for a hostname —
        // `allowedDevOrigins`, a cookie domain — rejects one with a port
        // in it.
        let values = injected(
            &config(SAMPLE),
            "myapp",
            &record("feat-1", false),
            Some("web"),
            &Gateway::with_ports(Some(8080), Some(8443)),
        );

        assert!(
            values
                .get("MINATO_URL_WEB")
                .is_some_and(|url| url.contains("8443")),
            "the URL carries the port: {:?}",
            values.get("MINATO_URL_WEB")
        );
        assert_eq!(
            values.get("MINATO_HOSTNAME_WEB").map(String::as_str),
            Some("web.feat-1.myapp.localhost"),
            "the hostname does not"
        );
    }

    #[test]
    fn main_workspace_urls_omit_the_label() {
        let values = injected(
            &config(SAMPLE),
            "myapp",
            &record("main", true),
            Some("web"),
            &Gateway::with_ports(Some(80), Some(443)),
        );

        assert_eq!(
            values.get("MINATO_URL_WEB").map(String::as_str),
            Some("https://web.myapp.localhost")
        );
    }

    #[test]
    fn a_shared_listing_names_no_service() {
        // There is no service to name, and putting one there would be
        // whichever happened to come first in the file.
        let values = injected(
            &config(SAMPLE),
            "myapp",
            &record("feat-1", false),
            None,
            &Gateway::inert(),
        );

        assert!(!values.contains_key("MINATO_SERVICE"));
        assert_eq!(
            values.get("MINATO_PROJECT").map(String::as_str),
            Some("myapp"),
            "the rest is shared and still belongs there"
        );
    }

    #[test]
    fn a_shared_listing_leaves_out_a_service_own_env() {
        // This is what it used to do: build the layers for whichever
        // service came first, so `web`'s own variables read as everyone's.
        let config = config(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            port = 3000
            env = { ONLY_WEB = "1" }
            [services.api]
            image = "node:22"
            port = 8080
        "#,
        );

        let shared = layers_for_service(
            &config,
            "myapp",
            &record("feat-1", false),
            std::path::Path::new("/repo"),
            None,
            &Paths::with_root(std::path::PathBuf::from("/nowhere")),
            &Gateway::inert(),
        )
        .expect("builds");

        assert!(
            !shared
                .resolve()
                .expect("resolves")
                .iter()
                .any(|entry| entry.key == "ONLY_WEB"),
            "one service's own env is not everyone's"
        );

        let web = layers_for_service(
            &config,
            "myapp",
            &record("feat-1", false),
            std::path::Path::new("/repo"),
            Some("web"),
            &Paths::with_root(std::path::PathBuf::from("/nowhere")),
            &Gateway::inert(),
        )
        .expect("builds");

        let own = web
            .resolve()
            .expect("resolves")
            .into_iter()
            .find(|entry| entry.key == "ONLY_WEB")
            .expect("asked about web, it is web's that matter");

        assert_eq!(
            own.scope,
            EnvScope::Service,
            "labelling it `project` sends someone to edit .minato/env for a \
             value the service overrides"
        );
    }

    #[test]
    fn a_service_can_put_its_url_under_the_name_its_app_reads() {
        // The whole point of expansion: no start-up script in between.
        let config = config(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            port = 3000
            env = { NEXT_PUBLIC_API_URL = "${MINATO_URL_API}" }
            [services.api]
            image = "node:22"
            port = 8080
        "#,
        );

        let layers = layers_for_service(
            &config,
            "myapp",
            &record("feat-1", false),
            std::path::Path::new("/repo"),
            Some("web"),
            &Paths::with_root(std::path::PathBuf::from("/nowhere")),
            &Gateway::with_ports(Some(80), Some(443)),
        )
        .expect("builds");

        let value = layers
            .resolve()
            .expect("resolves")
            .into_iter()
            .find(|entry| entry.key == "NEXT_PUBLIC_API_URL")
            .expect("present")
            .raw;

        assert_eq!(value, "https://api.feat-1.myapp.localhost");
    }

    #[test]
    fn a_url_that_is_missing_because_the_proxy_is_down_says_so() {
        // The configuration is right and the error is about the proxy.
        // "nothing sets it" alone sends someone to edit `minato.toml`.
        let config = config(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            port = 3000
            env = { NEXT_PUBLIC_API_URL = "${MINATO_URL_API}" }
            [services.api]
            image = "node:22"
            port = 8080
        "#,
        );

        let layers = layers_for_service(
            &config,
            "myapp",
            &record("feat-1", false),
            std::path::Path::new("/repo"),
            Some("web"),
            &Paths::with_root(std::path::PathBuf::from("/nowhere")),
            &Gateway::with_ports(None, None),
        )
        .expect("builds");

        let error = resolution_error(layers.resolve().expect_err("no URL to refer to"));

        assert!(
            error.hint.is_some_and(|hint| hint.contains("proxy")),
            "name the proxy, not the config"
        );
    }

    /// The error from settling a listing, with `project` as its project
    /// layer — the one a listing of no particular service does have.
    fn listing_failure(
        toml: &str,
        project: &str,
        service: Option<&str>,
    ) -> (env::EnvError, MinatoConfig, tempfile::TempDir) {
        let config = config(toml);

        let root = tempfile::tempdir().expect("tempdir");
        let path = env::project_env_path(root.path());
        std::fs::create_dir_all(path.parent().expect("has a parent")).expect("creates");
        std::fs::write(&path, project).expect("writes");

        let layers = layers_for_service(
            &config,
            "myapp",
            &record("feat-1", false),
            root.path(),
            service,
            &Paths::with_root(std::path::PathBuf::from("/nowhere")),
            &Gateway::inert(),
        )
        .expect("builds");

        let err = layers.resolve().expect_err("does not settle");
        (err, config, root)
    }

    #[test]
    fn a_listing_of_no_service_says_that_is_why() {
        // `MINATO_SERVICE` is left out of a shared listing on purpose, so
        // a value referring to it is right and it is the listing that
        // cannot settle. "Nothing sets it" would send someone hunting a
        // bug that is not there.
        let (err, config, _root) = listing_failure(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            port = 3000
        "#,
            "LOG_TAG=${MINATO_SERVICE}\n",
            None,
        );

        let note = listing_note(&err, None, &config);

        assert!(note.contains("names no service"), "{note}");
        assert!(note.contains("--service"), "say how to settle it: {note}");
    }

    #[test]
    fn a_value_only_one_service_defines_is_the_same_story() {
        let (err, config, _root) = listing_failure(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            port = 3000
            env = { OWN = "1" }
        "#,
            "DERIVED=${OWN}\n",
            None,
        );

        let note = listing_note(&err, None, &config);
        assert!(note.contains("names no service"), "{note}");
    }

    #[test]
    fn a_listing_about_one_service_gets_no_such_excuse() {
        // Asked about `web`, `MINATO_SERVICE` is there — so a name that
        // does not settle really is missing, and saying "this listing has
        // no service" would be a lie.
        let (err, config, _root) = listing_failure(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            port = 3000
        "#,
            "DERIVED=${NOWHERE}\n",
            Some("web"),
        );

        let note = listing_note(&err, Some("web"), &config);

        assert!(!note.contains("names no service"), "{note}");
        assert!(note.contains("NOWHERE"), "{note}");
        assert!(note.contains("shown as written"), "{note}");
    }

    #[test]
    fn a_missing_url_still_names_the_proxy_in_a_listing() {
        // The listing does not raise the error, so the hint that comes
        // with the error would never be seen without this.
        let (err, config, _root) = listing_failure(
            r#"
            [project]
            name = "myapp"
            [services.web]
            image = "node:22"
            port = 3000
            env = { API_URL = "${MINATO_URL_WEB}" }
        "#,
            "",
            Some("web"),
        );

        let note = listing_note(&err, Some("web"), &config);
        assert!(note.contains("proxy"), "{note}");
    }

    /// A worktree with nothing in it, and no git.
    fn worktree() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn writes_the_file_and_the_directories_leading_to_it() {
        let dir = worktree();
        let contents = env::render(&[], "service: api");

        let written = write_env_file(dir.path(), ".minato/env.api", &contents)
            .expect("writes")
            .expect("a new file is a change");

        assert_eq!(written, dir.path().join(".minato/env.api"));
        assert_eq!(
            std::fs::read_to_string(&written).expect("reads"),
            contents,
            "what was asked for, byte for byte"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&written)
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "it carries the environment");
        }
    }

    #[test]
    fn writing_the_same_contents_again_changes_nothing() {
        // Anything watching the file — a dev server, most of all — would
        // otherwise restart every time scale-to-zero woke the service.
        let dir = worktree();
        let contents = env::render(&[], "service: api");

        write_env_file(dir.path(), ".minato/env.api", &contents).expect("writes");
        let again = write_env_file(dir.path(), ".minato/env.api", &contents).expect("writes");

        assert!(again.is_none(), "unchanged is not a write");
    }

    #[test]
    fn replaces_a_file_it_wrote_itself() {
        let dir = worktree();

        write_env_file(dir.path(), ".env.minato", &env::render(&[], "old")).expect("writes");
        let new = env::render(&[], "new");
        write_env_file(dir.path(), ".env.minato", &new).expect("writes");

        assert_eq!(
            std::fs::read_to_string(dir.path().join(".env.minato")).expect("reads"),
            new
        );
    }

    #[test]
    fn never_overwrites_a_file_somebody_else_wrote() {
        // The whole risk of writing into a worktree: `.env.local` is a
        // path a person may already be using.
        let dir = worktree();
        let mine = "SECRET=mine\n";
        std::fs::write(dir.path().join(".env.local"), mine).expect("writes");

        let err = write_env_file(dir.path(), ".env.local", &env::render(&[], "service: web"))
            .unwrap_err()
            .to_string();

        assert!(err.contains("did not write"), "say why: {err}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".env.local")).expect("reads"),
            mine,
            "and leave it alone"
        );
    }

    #[test]
    fn never_overwrites_a_file_it_cannot_even_read() {
        // The marker cannot say a file is Minato's when the file will not
        // read as text at all — an `.env` in UTF-16, say. Unreadable is
        // still somebody's.
        let dir = worktree();
        let mine: &[u8] = &[0xff, 0xfe, b'F', 0x00, b'O', 0x00];
        std::fs::write(dir.path().join(".env.local"), mine).expect("writes");

        let err = write_env_file(dir.path(), ".env.local", &env::render(&[], "service: web"))
            .unwrap_err()
            .to_string();

        assert!(err.contains("did not write"), "say why: {err}");
        assert_eq!(
            std::fs::read(dir.path().join(".env.local")).expect("reads"),
            mine,
            "and leave it alone"
        );
    }

    #[test]
    fn does_not_write_through_a_leftover_temporary_file() {
        // A fixed temporary name is one an earlier crash can leave behind,
        // and the write opens rather than creates it: a leftover symlink
        // would carry the environment out of the worktree the containment
        // check just held it inside.
        let outside = tempfile::tempdir().expect("tempdir");
        let target = outside.path().join("stolen");
        std::fs::write(&target, "untouched").expect("writes");

        let dir = worktree();
        std::fs::create_dir(dir.path().join(".minato")).expect("creates");

        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, dir.path().join(".minato/.env.api.minato-tmp"))
            .expect("links");

        write_env_file(
            dir.path(),
            ".minato/env.api",
            &env::render(&[], "service: api"),
        )
        .expect("writes");

        assert_eq!(
            std::fs::read_to_string(&target).expect("reads"),
            "untouched",
            "the environment never went there"
        );
    }

    #[test]
    fn sweeps_temporaries_an_earlier_run_left_behind() {
        // A daemon killed between the write and the rename cannot clean up
        // after itself, and the next name is a different one — so without
        // this they gather in the worktree, one per crash.
        let dir = worktree();
        std::fs::create_dir(dir.path().join(".minato")).expect("creates");

        let stale = dir.path().join(".minato").join(format!(
            ".env.api.{}-1-0.minato-tmp",
            std::process::id() + 1
        ));
        std::fs::write(&stale, "half a file").expect("writes");

        write_env_file(
            dir.path(),
            ".minato/env.api",
            &env::render(&[], "service: api"),
        )
        .expect("writes");

        assert!(!stale.exists(), "the leftover is gone");
        assert!(dir.path().join(".minato/env.api").exists());
    }

    #[test]
    fn leaves_a_temporary_of_its_own_alone() {
        // One of this process's own may belong to a write happening right
        // now, and removing it would leave that write renaming a file that
        // is no longer there.
        let dir = worktree();
        std::fs::create_dir(dir.path().join(".minato")).expect("creates");

        let in_flight = dir
            .path()
            .join(".minato")
            .join(format!(".env.api.{}-1-0.minato-tmp", std::process::id()));
        std::fs::write(&in_flight, "being written").expect("writes");

        write_env_file(
            dir.path(),
            ".minato/env.api",
            &env::render(&[], "service: api"),
        )
        .expect("writes");

        assert!(in_flight.exists(), "not this process's to take");
    }

    #[test]
    fn refuses_a_path_that_leaves_the_worktree_through_a_symlink() {
        let outside = tempfile::tempdir().expect("tempdir");
        let dir = worktree();

        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).expect("links");

        let err = write_env_file(dir.path(), "escape/env", &env::render(&[], "service: web"))
            .unwrap_err()
            .to_string();

        assert!(err.contains("outside the worktree"), "{err}");
        assert!(
            !outside.path().join("env").exists(),
            "and nothing is written there"
        );
    }

    #[test]
    fn refuses_a_path_git_tracks() {
        // A generated file that git watches leaves the worktree dirty for
        // good, and committing it spreads one branch's URLs to every other.
        let dir = worktree();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .current_dir(dir.path())
                .args(args)
                .output()
                .expect("git runs")
        };

        git(&["init", "--quiet"]);
        std::fs::write(dir.path().join(".env"), "APP_ENV=development\n").expect("writes");
        git(&["add", ".env"]);

        let err = write_env_file(dir.path(), ".env", &env::render(&[], "service: web"))
            .unwrap_err()
            .to_string();

        assert!(err.contains("git tracks it"), "{err}");
    }

    #[test]
    fn url_variable_names_are_shell_safe() {
        assert_eq!(url_variable("web"), "MINATO_URL_WEB");
        assert_eq!(url_variable("api-server"), "MINATO_URL_API_SERVER");
    }
}
