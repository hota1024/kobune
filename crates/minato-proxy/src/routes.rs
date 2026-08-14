//! The table mapping hostnames to forwarding targets.
//!
//! Written by the daemon's supervisor, read by the proxy. The proxy knows
//! nothing about runtimes and simply forwards to [`Route::endpoint`] —
//! a forwarded host port under Docker, the container's own IP under Apple
//! Container.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

/// Where one hostname forwards to.
///
/// **Stopped services are registered too.** Scale-to-zero has to tell
/// "stopped" apart from "does not exist": the first is woken by a request,
/// the second gets a 404.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// The forwarding target. `None` while stopped.
    pub endpoint: Option<SocketAddr>,
    /// Identifiers, for diagnostics and logs.
    pub project: String,
    pub workspace: String,
    pub service: String,
}

impl Route {
    /// A running service.
    pub fn new(
        endpoint: SocketAddr,
        project: impl Into<String>,
        workspace: impl Into<String>,
        service: impl Into<String>,
    ) -> Self {
        Self {
            endpoint: Some(endpoint),
            project: project.into(),
            workspace: workspace.into(),
            service: service.into(),
        }
    }

    /// A stopped service, to be woken by a request.
    pub fn stopped(
        project: impl Into<String>,
        workspace: impl Into<String>,
        service: impl Into<String>,
    ) -> Self {
        Self {
            endpoint: None,
            project: project.into(),
            workspace: workspace.into(),
            service: service.into(),
        }
    }

    pub fn is_running(&self) -> bool {
        self.endpoint.is_some()
    }
}

/// The routing table, shared across threads.
///
/// Reads dominate (one per request) and writes are rare (a service
/// starting or stopping), so an `RwLock` is enough.
#[derive(Clone, Default)]
pub struct Routes {
    inner: Arc<RwLock<HashMap<String, Route>>>,
}

impl Routes {
    pub fn new() -> Self {
        Self::default()
    }

    /// Looks up a target. `host` need not be normalised.
    pub fn get(&self, host: &str) -> Option<Route> {
        let key = normalize_host(host)?;
        self.inner
            .read()
            .expect("the routing table lock is poisoned")
            .get(&key)
            .cloned()
    }

    pub fn insert(&self, host: &str, route: Route) {
        let Some(key) = normalize_host(host) else {
            tracing::warn!("not a usable hostname, so not registered: {host}");
            return;
        };

        self.inner
            .write()
            .expect("the routing table lock is poisoned")
            .insert(key, route);
    }

    pub fn remove(&self, host: &str) {
        let Some(key) = normalize_host(host) else {
            return;
        };

        self.inner
            .write()
            .expect("the routing table lock is poisoned")
            .remove(&key);
    }

    /// Replaces every route of one project at once.
    ///
    /// Re-reading the state and swapping wholesale misses nothing, unlike
    /// tracking individual additions and removals. It also matches the
    /// rule that the runtime's labels are the source of truth.
    pub fn replace_project(&self, project: &str, entries: Vec<(String, Route)>) {
        let mut guard = self
            .inner
            .write()
            .expect("the routing table lock is poisoned");

        guard.retain(|_, route| route.project != project);

        for (host, route) in entries {
            if let Some(key) = normalize_host(&host) {
                // Two projects can produce the same tunnel hostname, since
                // that joins project, workspace and service into one label
                // (`minato_core::naming::tunnel_host`). Whoever refreshed
                // last would otherwise serve both URLs with nothing said.
                if let Some(taken) = guard.get(&key)
                    && taken.project != route.project
                {
                    tracing::warn!(
                        "{key} is claimed by project `{}` and project `{}`; \
                         it now serves the latter",
                        taken.project,
                        route.project
                    );
                }

                guard.insert(key, route);
            }
        }
    }

    /// Every registered host and target. For diagnostics.
    pub fn snapshot(&self) -> Vec<(String, Route)> {
        let guard = self
            .inner
            .read()
            .expect("the routing table lock is poisoned");

        let mut entries: Vec<(String, Route)> = guard
            .iter()
            .map(|(host, route)| (host.clone(), route.clone()))
            .collect();

        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries
    }

    pub fn len(&self) -> usize {
        self.inner
            .read()
            .expect("the routing table lock is poisoned")
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Normalises a `Host` header or SNI name into a table key.
///
/// Browsers send the port along (`web.feat-1.myapp.localhost:8080`) and
/// names from DNS can carry a trailing dot. Case is not significant.
pub fn normalize_host(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    // For an IPv6 literal (`[::1]:8080`), take what is inside the brackets.
    let without_port = if let Some(rest) = trimmed.strip_prefix('[') {
        let end = rest.find(']')?;
        &rest[..end]
    } else {
        match trimmed.split_once(':') {
            Some((host, _port)) => host,
            None => trimmed,
        }
    };

    // Drop the trailing dot of a fully-qualified name.
    let without_dot = without_port.trim_end_matches('.');
    if without_dot.is_empty() {
        return None;
    }

    Some(without_dot.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(port: u16) -> Route {
        Route::new(
            SocketAddr::from(([127, 0, 0, 1], port)),
            "myapp",
            "feat-1",
            "web",
        )
    }

    #[test]
    fn strips_port_from_host_header() {
        // On a non-standard port the browser includes it in Host.
        assert_eq!(
            normalize_host("web.feat-1.myapp.localhost:8443").as_deref(),
            Some("web.feat-1.myapp.localhost")
        );
    }

    #[test]
    fn lowercases_and_strips_trailing_dot() {
        assert_eq!(
            normalize_host("WEB.Feat-1.MyApp.localhost.").as_deref(),
            Some("web.feat-1.myapp.localhost")
        );
    }

    #[test]
    fn handles_ipv6_literals() {
        assert_eq!(normalize_host("[::1]:8080").as_deref(), Some("::1"));
        assert_eq!(normalize_host("[::1]").as_deref(), Some("::1"));
    }

    #[test]
    fn rejects_empty_hosts() {
        assert_eq!(normalize_host(""), None);
        assert_eq!(normalize_host("   "), None);
        assert_eq!(normalize_host("."), None);
        assert_eq!(normalize_host(":8080"), None);
    }

    #[test]
    fn lookup_normalizes_the_query() {
        let routes = Routes::new();
        routes.insert("web.feat-1.myapp.localhost", route(3000));

        // Both sides are normalised, so any spelling resolves.
        assert!(routes.get("web.feat-1.myapp.localhost").is_some());
        assert!(routes.get("WEB.feat-1.myapp.localhost:443").is_some());
        assert!(routes.get("web.feat-1.myapp.localhost.").is_some());
        assert!(routes.get("other.localhost").is_none());
    }

    #[test]
    fn remove_deletes_the_entry() {
        let routes = Routes::new();
        routes.insert("web.feat-1.myapp.localhost", route(3000));
        routes.remove("WEB.feat-1.myapp.localhost:80");

        assert!(routes.is_empty());
    }

    #[test]
    fn replace_project_swaps_only_that_project() {
        let routes = Routes::new();
        routes.insert("web.feat-1.myapp.localhost", route(3000));
        routes.insert(
            "web.other.localhost",
            Route::new(
                SocketAddr::from(([127, 0, 0, 1], 9000)),
                "other",
                "main",
                "web",
            ),
        );

        routes.replace_project(
            "myapp",
            vec![(
                "api.feat-2.myapp.localhost".to_string(),
                Route::new(
                    SocketAddr::from(([127, 0, 0, 1], 4000)),
                    "myapp",
                    "feat-2",
                    "api",
                ),
            )],
        );

        assert!(
            routes.get("web.feat-1.myapp.localhost").is_none(),
            "the old routes are gone"
        );
        assert!(routes.get("api.feat-2.myapp.localhost").is_some());
        assert!(
            routes.get("web.other.localhost").is_some(),
            "other projects are untouched"
        );
    }

    #[test]
    fn a_hostname_two_projects_both_claim_goes_to_the_later_one() {
        // Tunnel hostnames put the project inside a single label, so two
        // projects can produce the same one. Pinning the behaviour down:
        // last write wins, deterministically, and the daemon log says so.
        let routes = Routes::new();
        routes.insert(
            "web-myapp-x.example.com",
            Route::new(
                SocketAddr::from(([127, 0, 0, 1], 3000)),
                "x",
                "myapp",
                "web",
            ),
        );

        routes.replace_project(
            "myapp-x",
            vec![(
                "web-myapp-x.example.com".to_string(),
                Route::new(
                    SocketAddr::from(([127, 0, 0, 1], 4000)),
                    "myapp-x",
                    "main",
                    "web",
                ),
            )],
        );

        let route = routes.get("web-myapp-x.example.com").expect("registered");
        assert_eq!(route.project, "myapp-x", "the later refresh holds it");
    }

    #[test]
    fn replace_project_with_nothing_clears_it() {
        let routes = Routes::new();
        routes.insert("web.feat-1.myapp.localhost", route(3000));

        routes.replace_project("myapp", vec![]);
        assert!(routes.is_empty(), "stopping everything leaves no routes");
    }

    #[test]
    fn stopped_routes_are_registered_without_an_endpoint() {
        // Without telling "stopped" from "does not exist", a stopped
        // service can never be woken and just 404s.
        let routes = Routes::new();
        routes.insert(
            "web.feat-1.myapp.localhost",
            Route::stopped("myapp", "feat-1", "web"),
        );

        let route = routes
            .get("web.feat-1.myapp.localhost")
            .expect("registered");
        assert!(!route.is_running());
        assert_eq!(route.endpoint, None);

        assert!(
            routes.get("never-created.myapp.localhost").is_none(),
            "an unknown host is a different matter"
        );
    }

    #[test]
    fn snapshot_is_sorted_for_stable_output() {
        let routes = Routes::new();
        routes.insert("web.feat-1.myapp.localhost", route(3000));
        routes.insert("api.feat-1.myapp.localhost", route(4000));

        let hosts: Vec<String> = routes.snapshot().into_iter().map(|(h, _)| h).collect();
        assert_eq!(
            hosts,
            vec!["api.feat-1.myapp.localhost", "web.feat-1.myapp.localhost"]
        );
    }
}
