//! Building the environment a service receives.
//!
//! The layers stack as `docs/DESIGN.md` §8 describes. What Minato injects
//! goes first, so the user's own settings win. The other way round,
//! Minato's conveniences would quietly erase them.

use indexmap::IndexMap;
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

    for (name, _, url) in exposed_urls(config, record, gateway) {
        values.insert(url_variable(&name), url);
    }

    values
}

/// The hostnames the workspace's services answer to.
///
/// What a container has to be able to resolve for the URL it was handed to
/// work from inside one — see `ServiceSpec::gateway_hosts`. The same names
/// `MINATO_URL_<SERVICE>` is built from, under the same condition: no
/// proxy, no URL, and so nothing to point anywhere.
pub fn service_hosts(
    config: &MinatoConfig,
    record: &WorkspaceRecord,
    gateway: &Gateway,
) -> Vec<String> {
    exposed_urls(config, record, gateway)
        .into_iter()
        .map(|(_, host, _)| host)
        .collect()
}

/// Every exposed service, as `(name, hostname, URL)`.
///
/// **The one place that decides which services have a URL at all.** The
/// variables and the hostnames a container resolves have to agree on that
/// set; a name pointed at the proxy for a service the proxy does not route
/// resolves to a 404 rather than to nothing.
fn exposed_urls(
    config: &MinatoConfig,
    record: &WorkspaceRecord,
    gateway: &Gateway,
) -> Vec<(String, String, String)> {
    let domain = config.domain();

    config
        .services
        .iter()
        .filter(|(_, service)| service.exposed())
        .filter_map(|(name, _)| {
            let host = minato_core::naming::service_host_in(name, record.url_label(), &domain);

            // With no proxy running there is no URL. An empty string would
            // leave it "set, but broken".
            let url = gateway.url_for(&host)?;

            Some((name.clone(), host, url))
        })
        .collect()
}

/// Turns a service name into a variable name.
///
/// `cache-store` becomes `MINATO_URL_CACHE_STORE`. A hyphen is not valid
/// in a variable name, so it becomes an underscore.
pub fn url_variable(service: &str) -> String {
    format!("MINATO_URL_{}", service.to_uppercase().replace('-', "_"))
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
    fn lists_the_hostnames_a_container_has_to_resolve() {
        // The same names the URLs are built from: a container calling the
        // URL it was handed has to reach the proxy, not NXDOMAIN.
        let hosts = service_hosts(
            &config(SAMPLE),
            &record("feat-1", false),
            &Gateway::with_ports(Some(80), Some(443)),
        );

        assert_eq!(
            hosts,
            vec![
                "web.feat-1.myapp.localhost".to_string(),
                "api-server.feat-1.myapp.localhost".to_string(),
            ],
            "every exposed service, and `db` is not one"
        );
    }

    #[test]
    fn the_main_worktree_keeps_its_hostnames_short() {
        let hosts = service_hosts(
            &config(SAMPLE),
            &record("main", true),
            &Gateway::with_ports(Some(80), Some(443)),
        );

        assert!(
            hosts.contains(&"web.myapp.localhost".to_string()),
            "no workspace label on the main worktree: {hosts:?}"
        );
    }

    #[test]
    fn no_proxy_means_no_hostnames_to_point_anywhere() {
        // Pointing a name at a proxy that is not running would turn a
        // clean NXDOMAIN into a connection refused, which reads as the
        // service being broken rather than absent.
        assert!(
            service_hosts(&config(SAMPLE), &record("feat-1", false), &Gateway::inert()).is_empty()
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
            !shared.resolve().iter().any(|entry| entry.key == "ONLY_WEB"),
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
    fn url_variable_names_are_shell_safe() {
        assert_eq!(url_variable("web"), "MINATO_URL_WEB");
        assert_eq!(url_variable("api-server"), "MINATO_URL_API_SERVER");
    }
}
