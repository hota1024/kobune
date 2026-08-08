//! The way in to an environment: the proxy's and DNS's listeners, in one
//! place.
//!
//! A failed bind does not take the daemon down. 80 and 443 are privileged,
//! and machines without that permission are common enough. Issuing no URLs
//! and pointing at the raw `endpoint` instead beats nothing working at
//! all.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use crate::activation;
use minato_core::Paths;
use minato_dns::DnsConfig;
use minato_proxy::{Activator, LocalCa, Routes, serve_http, serve_https, server_config};
use tokio::net::TcpListener;
use tokio::sync::Notify;

/// The environment variables that override the listening ports.
pub const HTTP_PORT_ENV: &str = "MINATO_HTTP_PORT";
pub const HTTPS_PORT_ENV: &str = "MINATO_HTTPS_PORT";
pub const DNS_PORT_ENV: &str = "MINATO_DNS_PORT";

/// The default DNS port.
pub const DEFAULT_DNS_PORT: u16 = 53;

#[derive(Debug, Clone)]
pub struct GatewaySettings {
    pub http_port: u16,
    pub https_port: u16,
    pub dns_port: u16,
    /// Where the proxy listens.
    ///
    /// **Both loopback families.** macOS resolves `*.localhost` to `::1`
    /// and `127.0.0.1` alike, and clients prefer IPv6. Holding only one of
    /// them silently routes traffic to whatever unrelated app happens to
    /// be on `[::1]`.
    ///
    /// Loopback only: this is for local development, and 0.0.0.0 would put
    /// the environment in front of everyone else on the LAN.
    pub bind: Vec<IpAddr>,
    /// Where DNS listens. The resolver configuration names 127.0.0.1
    /// outright, so there is no ambiguity and IPv4 alone will do.
    pub dns_bind: IpAddr,
}

impl Default for GatewaySettings {
    fn default() -> Self {
        Self {
            http_port: minato_proxy::DEFAULT_HTTP_PORT,
            https_port: minato_proxy::DEFAULT_HTTPS_PORT,
            dns_port: DEFAULT_DNS_PORT,
            bind: vec![
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                IpAddr::V6(Ipv6Addr::LOCALHOST),
            ],
            dns_bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
        }
    }
}

impl GatewaySettings {
    /// Overrides from the environment, for staying off privileged ports.
    pub fn from_env() -> Self {
        let mut settings = Self::default();

        if let Some(port) = port_from_env(HTTP_PORT_ENV) {
            settings.http_port = port;
        }
        if let Some(port) = port_from_env(HTTPS_PORT_ENV) {
            settings.https_port = port;
        }
        if let Some(port) = port_from_env(DNS_PORT_ENV) {
            settings.dns_port = port;
        }

        settings
    }
}

fn port_from_env(key: &str) -> Option<u16> {
    let raw = std::env::var(key).ok()?;
    match raw.parse::<u16>() {
        Ok(port) => Some(port),
        Err(_) => {
            tracing::warn!("{key}'s value `{raw}` is not a port number");
            None
        }
    }
}

/// A running gateway.
pub struct Gateway {
    routes: Routes,
    /// The addresses actually bound.
    ///
    /// Whole addresses rather than ports, **so a bind that only got one
    /// family can be spotted**. `*.localhost` resolves to both `::1` and
    /// `127.0.0.1`, and missing IPv6 hands requests to a different app —
    /// which is exactly what happened.
    http_addrs: Vec<SocketAddr>,
    https_addrs: Vec<SocketAddr>,
    dns_port: Option<u16>,
    ca_path: Option<PathBuf>,
    /// The addresses that were meant to be bound, for working out which
    /// were missed.
    wanted: Vec<IpAddr>,
}

impl Gateway {
    /// Starts the proxy and DNS. Whatever fails stays off, and the rest
    /// carries on.
    pub async fn start(
        paths: &Paths,
        settings: &GatewaySettings,
        activator: Arc<dyn Activator>,
        shutdown: Arc<Notify>,
    ) -> Self {
        let routes = Routes::new();

        let http_addrs =
            Self::start_http(&routes, settings, activator.clone(), shutdown.clone()).await;
        let (https_addrs, ca_path) =
            Self::start_https(paths, &routes, settings, activator, shutdown.clone()).await;
        let dns_port = Self::start_dns(settings, shutdown).await;

        Self {
            routes,
            http_addrs,
            https_addrs,
            dns_port,
            ca_path,
            wanted: settings.bind.clone(),
        }
    }

    async fn start_http(
        routes: &Routes,
        settings: &GatewaySettings,
        activator: Arc<dyn Activator>,
        shutdown: Arc<Notify>,
    ) -> Vec<SocketAddr> {
        // Use launchd's already-bound socket when there is one. 80 is
        // privileged, so unprivileged there is no other way to hold it.
        let activated = adopt_listeners(activation::HTTP_SOCKET);
        if !activated.is_empty() {
            let addrs: Vec<SocketAddr> = activated
                .iter()
                .filter_map(|l| l.local_addr().ok())
                .collect();

            for listener in activated {
                let routes = routes.clone();
                let activator = activator.clone();
                let shutdown = shutdown.clone();
                tokio::spawn(async move {
                    if let Err(err) = serve_http(listener, routes, activator, shutdown).await {
                        tracing::warn!("the HTTP proxy stopped: {err}");
                    }
                });
            }

            tracing::info!("HTTP proxy: inherited from launchd ({addrs:?})");
            return addrs;
        }

        let mut bound = Vec::new();

        for ip in &settings.bind {
            let addr = SocketAddr::new(*ip, settings.http_port);

            match TcpListener::bind(addr).await {
                Ok(listener) => {
                    bound.push(listener.local_addr().unwrap_or(addr));

                    let routes = routes.clone();
                    let activator = activator.clone();
                    let shutdown = shutdown.clone();

                    tokio::spawn(async move {
                        if let Err(err) = serve_http(listener, routes, activator, shutdown).await {
                            tracing::warn!("the HTTP proxy stopped: {err}");
                        }
                    });

                    tracing::info!("HTTP proxy: {addr}");
                }
                Err(err) => report_bind_failure("the HTTP proxy", addr, &err),
            }
        }

        bound
    }

    async fn start_https(
        paths: &Paths,
        routes: &Routes,
        settings: &GatewaySettings,
        activator: Arc<dyn Activator>,
        shutdown: Arc<Notify>,
    ) -> (Vec<SocketAddr>, Option<PathBuf>) {
        let ca = match LocalCa::load_or_create(&paths.ca_dir()) {
            Ok(ca) => Arc::new(ca),
            Err(err) => {
                tracing::warn!("no CA available, so HTTPS stays off: {err}");
                return (Vec::new(), None);
            }
        };

        let ca_path = ca.certificate_path();
        let tls = server_config(ca);

        let activated = adopt_listeners(activation::HTTPS_SOCKET);
        if !activated.is_empty() {
            let addrs: Vec<SocketAddr> = activated
                .iter()
                .filter_map(|l| l.local_addr().ok())
                .collect();

            for listener in activated {
                let routes = routes.clone();
                let activator = activator.clone();
                let shutdown = shutdown.clone();
                let tls = tls.clone();
                tokio::spawn(async move {
                    if let Err(err) = serve_https(listener, routes, activator, tls, shutdown).await
                    {
                        tracing::warn!("the HTTPS proxy stopped: {err}");
                    }
                });
            }

            tracing::info!("HTTPS proxy: inherited from launchd ({addrs:?})");
            return (addrs, Some(ca_path));
        }

        let mut bound = Vec::new();

        for ip in &settings.bind {
            let addr = SocketAddr::new(*ip, settings.https_port);

            match TcpListener::bind(addr).await {
                Ok(listener) => {
                    bound.push(listener.local_addr().unwrap_or(addr));

                    let routes = routes.clone();
                    let activator = activator.clone();
                    let shutdown = shutdown.clone();
                    let tls = tls.clone();

                    tokio::spawn(async move {
                        if let Err(err) =
                            serve_https(listener, routes, activator, tls, shutdown).await
                        {
                            tracing::warn!("the HTTPS proxy stopped: {err}");
                        }
                    });

                    tracing::info!("HTTPS proxy: {addr}");
                }
                Err(err) => report_bind_failure("the HTTPS proxy", addr, &err),
            }
        }

        (bound, Some(ca_path))
    }

    async fn start_dns(settings: &GatewaySettings, shutdown: Arc<Notify>) -> Option<u16> {
        // :53 is privileged too. Use launchd's if it has one.
        let udp = adopt_udp(activation::DNS_UDP_SOCKET);
        let tcp = adopt_listeners(activation::DNS_TCP_SOCKET);

        if !udp.is_empty() || !tcp.is_empty() {
            let port = udp
                .first()
                .and_then(|s| s.local_addr().ok())
                .map(|a| a.port())
                .or_else(|| {
                    tcp.first()
                        .and_then(|l| l.local_addr().ok())
                        .map(|a| a.port())
                });

            let config = DnsConfig::default();
            tokio::spawn(async move {
                if let Err(err) = minato_dns::serve_sockets(udp, tcp, config, shutdown).await {
                    tracing::warn!("the DNS server stopped: {err}");
                }
            });

            tracing::info!("DNS: inherited from launchd (:{})", port.unwrap_or(0));
            return port.or(Some(settings.dns_port));
        }

        let addr = SocketAddr::new(settings.dns_bind, settings.dns_port);

        // Check the bind first. A failure inside serve() would leave the
        // caller unable to tell whether it started.
        match tokio::net::UdpSocket::bind(addr).await {
            Ok(socket) => drop(socket),
            Err(err) => {
                report_bind_failure("DNS", addr, &err);
                return None;
            }
        }

        let config = DnsConfig::default();
        tokio::spawn(async move {
            if let Err(err) = minato_dns::serve(addr, config, shutdown).await {
                tracing::warn!("the DNS server stopped: {err}");
            }
        });

        tracing::info!("DNS: {addr}");
        Some(settings.dns_port)
    }

    /// A gateway listening on nothing — the same as every bind failing.
    #[cfg(test)]
    pub(crate) fn inert() -> Self {
        Self {
            routes: Routes::new(),
            http_addrs: Vec::new(),
            https_addrs: Vec::new(),
            dns_port: None,
            ca_path: None,
            wanted: Vec::new(),
        }
    }

    /// A gateway with fixed ports, for testing how URLs are built.
    #[cfg(test)]
    pub(crate) fn with_ports(http: Option<u16>, https: Option<u16>) -> Self {
        let both = |port: u16| {
            vec![
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
                SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port),
            ]
        };

        Self {
            routes: Routes::new(),
            http_addrs: http.map(both).unwrap_or_default(),
            https_addrs: https.map(both).unwrap_or_default(),
            dns_port: None,
            ca_path: None,
            wanted: vec![
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                IpAddr::V6(Ipv6Addr::LOCALHOST),
            ],
        }
    }

    pub fn routes(&self) -> &Routes {
        &self.routes
    }

    pub fn http_port(&self) -> Option<u16> {
        self.http_addrs.first().map(|addr| addr.port())
    }

    pub fn https_port(&self) -> Option<u16> {
        self.https_addrs.first().map(|addr| addr.port())
    }

    /// The address families that were wanted but not bound.
    ///
    /// Anything here means requests to that address reach some other
    /// process. `*.localhost` resolves to both, so the damage is real.
    pub fn missing_families(&self) -> Vec<IpAddr> {
        if self.http_addrs.is_empty() && self.https_addrs.is_empty() {
            return Vec::new();
        }

        let bound: Vec<IpAddr> = self
            .http_addrs
            .iter()
            .chain(self.https_addrs.iter())
            .map(|addr| addr.ip())
            .collect();

        self.wanted
            .iter()
            .filter(|wanted| !bound.iter().any(|got| got.is_ipv4() == wanted.is_ipv4()))
            .copied()
            .collect()
    }

    pub fn dns_port(&self) -> Option<u16> {
        self.dns_port
    }

    pub fn ca_path(&self) -> Option<&std::path::Path> {
        self.ca_path.as_deref()
    }

    /// Whether the proxy is running. No URLs are issued when it is not.
    pub fn is_serving(&self) -> bool {
        !self.http_addrs.is_empty() || !self.https_addrs.is_empty()
    }

    /// The URL for a hostname, or `None` when the proxy is not running.
    ///
    /// HTTPS wins: browsers apply mixed-content and Secure-cookie rules to
    /// a plain HTTP dev server too.
    pub fn url_for(&self, host: &str) -> Option<String> {
        if let Some(port) = self.https_port() {
            return Some(format_url("https", host, port, 443));
        }

        self.http_port()
            .map(|port| format_url("http", host, port, 80))
    }
}

/// Builds a URL, leaving the port off when it is the default.
fn format_url(scheme: &str, host: &str, port: u16, default_port: u16) -> String {
    if port == default_port {
        format!("{scheme}://{host}")
    } else {
        format!("{scheme}://{host}:{port}")
    }
}

/// Turns a descriptor from launchd into a tokio listener.
fn adopt_listeners(name: &str) -> Vec<TcpListener> {
    activation::tcp_listeners(name)
        .into_iter()
        .filter_map(|listener| match TcpListener::from_std(listener) {
            Ok(listener) => Some(listener),
            Err(err) => {
                tracing::warn!("cannot take over {name}'s socket: {err}");
                None
            }
        })
        .collect()
}

fn adopt_udp(name: &str) -> Vec<tokio::net::UdpSocket> {
    activation::udp_sockets(name)
        .into_iter()
        .filter_map(|socket| match tokio::net::UdpSocket::from_std(socket) {
            Ok(socket) => Some(socket),
            Err(err) => {
                tracing::warn!("cannot take over {name}'s socket: {err}");
                None
            }
        })
        .collect()
}

fn report_bind_failure(what: &str, addr: SocketAddr, err: &std::io::Error) {
    if err.kind() == std::io::ErrorKind::PermissionDenied && addr.port() < 1024 {
        tracing::warn!(
            "{what} cannot hold {addr} (a port below 1024 needs privileges). \
             `minato doctor` says what to do about it"
        );
    } else if err.kind() == std::io::ErrorKind::AddrInUse {
        tracing::warn!(
            "{what} cannot hold {addr} (another process has it). \
             Check `minato doctor`"
        );
    } else {
        tracing::warn!("{what} cannot hold {addr}: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gateway(http: Option<u16>, https: Option<u16>) -> Gateway {
        Gateway::with_ports(http, https)
    }

    #[test]
    fn prefers_https_and_omits_the_default_port() {
        let gateway = gateway(Some(80), Some(443));
        assert_eq!(
            gateway.url_for("web.feat-1.myapp.localhost").as_deref(),
            Some("https://web.feat-1.myapp.localhost")
        );
    }

    #[test]
    fn includes_non_default_ports() {
        let gateway = gateway(Some(8080), Some(8443));
        assert_eq!(
            gateway.url_for("web.myapp.localhost").as_deref(),
            Some("https://web.myapp.localhost:8443")
        );
    }

    #[test]
    fn falls_back_to_http_when_tls_is_unavailable() {
        let gateway = gateway(Some(8080), None);
        assert_eq!(
            gateway.url_for("web.myapp.localhost").as_deref(),
            Some("http://web.myapp.localhost:8080")
        );
    }

    #[test]
    fn issues_no_url_when_nothing_is_listening() {
        // A URL with nothing listening behind it points at a dead end.
        let gateway = gateway(None, None);

        assert!(!gateway.is_serving());
        assert_eq!(gateway.url_for("web.myapp.localhost"), None);
    }

    #[test]
    fn defaults_to_privileged_ports_on_loopback() {
        let settings = GatewaySettings::default();

        assert_eq!(settings.http_port, 80);
        assert_eq!(settings.https_port, 443);
        assert_eq!(settings.dns_port, 53);
        assert_eq!(
            settings.dns_bind,
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            "0.0.0.0 would put the environment in front of the LAN"
        );
    }

    #[test]
    fn detects_a_missing_address_family() {
        // With another app holding [::1]:8080, only IPv4 is available
        // here. *.localhost resolves to both, so anything arriving over
        // IPv6 goes to that app instead. This must not pass silently.
        let gateway = Gateway {
            routes: Routes::new(),
            http_addrs: vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080)],
            https_addrs: Vec::new(),
            dns_port: None,
            ca_path: None,
            wanted: vec![
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                IpAddr::V6(Ipv6Addr::LOCALHOST),
            ],
        };

        assert_eq!(
            gateway.missing_families(),
            vec![IpAddr::V6(Ipv6Addr::LOCALHOST)]
        );
    }

    #[test]
    fn no_missing_families_when_both_are_bound() {
        assert!(
            Gateway::with_ports(Some(80), Some(443))
                .missing_families()
                .is_empty()
        );
    }

    #[test]
    fn nothing_bound_is_reported_separately() {
        // Everything failing is not the "only one family" problem.
        assert!(Gateway::inert().missing_families().is_empty());
    }

    #[test]
    fn listens_on_both_loopback_families() {
        // *.localhost resolves to both ::1 and 127.0.0.1. Holding one
        // sends the rest to whatever app is on the other.
        let settings = GatewaySettings::default();

        assert!(settings.bind.contains(&IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(settings.bind.contains(&IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(
            settings.bind.iter().all(|ip| ip.is_loopback()),
            "nothing outside loopback is exposed"
        );
    }
}
