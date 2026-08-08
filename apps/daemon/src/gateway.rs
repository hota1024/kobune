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

/// Where the proxy goes when it cannot have 80 and 443.
///
/// **A proxy on an awkward port beats no proxy at all.** Without one no URL
/// is issued, which also means no `MINATO_URL_<SERVICE>` reaches a
/// container — and inside one that surfaces as `parameter not set`, naming
/// nothing that leads back to a privilege the daemon never had.
///
/// Not 8080/8443: a dev server sits there often enough that taking it from
/// the user's own app would be a worse failure than the one being avoided.
/// Fixed rather than left to the OS, so a URL survives a daemon restart.
///
/// There is no DNS equivalent. `/etc/resolver` names the port and writing
/// it needs root, so moving DNS off 53 without that achieves nothing.
pub const FALLBACK_HTTP_PORT: u16 = 18080;
pub const FALLBACK_HTTPS_PORT: u16 = 18443;

/// Why a listener could not be held.
///
/// **Worth keeping, not just logging.** "Needs privileges" and "something
/// else has it" want opposite fixes, and after `minato daemon stop` the
/// second one is what happens — launchd keeps the socket while the job is
/// idle. Reporting that as a privileges problem sends people back to
/// `minato setup`, which they have already run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindFailure {
    /// A port below 1024, without the privileges to hold it.
    Privileged,
    /// Another process has it.
    InUse,
    Other,
}

impl BindFailure {
    fn classify(err: &std::io::Error, addr: SocketAddr) -> Self {
        match err.kind() {
            std::io::ErrorKind::PermissionDenied if addr.port() < 1024 => Self::Privileged,
            std::io::ErrorKind::AddrInUse => Self::InUse,
            _ => Self::Other,
        }
    }

    /// What to show as the check's detail.
    pub fn detail(self) -> &'static str {
        match self {
            Self::Privileged => "not listening (the port needs privileges)",
            Self::InUse => "not listening (another process holds the port)",
            Self::Other => "not listening (the port could not be held)",
        }
    }
}

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
    /// Whether the proxy ports were asked for by name.
    ///
    /// **A port someone named is not fallen back from.** Silently listening
    /// somewhere else would ignore the instruction, and the whole reason to
    /// set `MINATO_HTTP_PORT` is to decide this yourself.
    pub http_port_named: bool,
    pub https_port_named: bool,
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
            http_port_named: false,
            https_port_named: false,
        }
    }
}

impl GatewaySettings {
    /// Overrides from the environment, for staying off privileged ports.
    pub fn from_env() -> Self {
        let mut settings = Self::default();

        if let Some(port) = port_from_env(HTTP_PORT_ENV) {
            settings.http_port = port;
            settings.http_port_named = true;
        }
        if let Some(port) = port_from_env(HTTPS_PORT_ENV) {
            settings.https_port = port;
            settings.https_port_named = true;
        }
        if let Some(port) = port_from_env(DNS_PORT_ENV) {
            settings.dns_port = port;
        }

        settings
    }

    /// The port to try after `port`, if there is one worth trying.
    ///
    /// `None` in the two cases where moving is wrong rather than merely
    /// unnecessary: the port was named, or launchd is holding the
    /// privileged one for a job that is simply not running. Listening
    /// elsewhere in that second case would leave the machine looking
    /// healthy while socket activation stays broken.
    ///
    /// `launchd_installed` is passed in rather than read here, so the rule
    /// can be checked without a plist on the machine running the tests.
    fn fallback(
        &self,
        named: bool,
        port: u16,
        fallback: u16,
        launchd_installed: bool,
    ) -> Option<u16> {
        if named || port == fallback || launchd_installed {
            return None;
        }

        Some(fallback)
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
    /// Why each listener is missing, when it is.
    http_failure: Option<BindFailure>,
    https_failure: Option<BindFailure>,
    dns_failure: Option<BindFailure>,
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

        // Read once, and passed down: it decides whether a refused port is
        // worth moving away from, and both listeners have to agree.
        let launchd_installed = minato_core::launchd::is_installed();

        let (http_addrs, http_failure) = Self::start_http(
            &routes,
            settings,
            launchd_installed,
            activator.clone(),
            shutdown.clone(),
        )
        .await;
        let (https_addrs, ca_path, https_failure) = Self::start_https(
            paths,
            &routes,
            settings,
            launchd_installed,
            activator,
            shutdown.clone(),
        )
        .await;
        let (dns_port, dns_failure) = Self::start_dns(settings, shutdown).await;

        Self {
            routes,
            http_addrs,
            https_addrs,
            dns_port,
            ca_path,
            wanted: settings.bind.clone(),
            http_failure,
            https_failure,
            dns_failure,
        }
    }

    async fn start_http(
        routes: &Routes,
        settings: &GatewaySettings,
        launchd_installed: bool,
        activator: Arc<dyn Activator>,
        shutdown: Arc<Notify>,
    ) -> (Vec<SocketAddr>, Option<BindFailure>) {
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
            return (addrs, None);
        }

        let (listeners, failure) = bind_with_fallback(
            settings,
            "the HTTP proxy",
            settings.http_port,
            settings.fallback(
                settings.http_port_named,
                settings.http_port,
                FALLBACK_HTTP_PORT,
                launchd_installed,
            ),
        )
        .await;

        let mut bound = Vec::with_capacity(listeners.len());

        for listener in listeners {
            bound.extend(listener.local_addr().ok());

            let routes = routes.clone();
            let activator = activator.clone();
            let shutdown = shutdown.clone();

            tokio::spawn(async move {
                if let Err(err) = serve_http(listener, routes, activator, shutdown).await {
                    tracing::warn!("the HTTP proxy stopped: {err}");
                }
            });
        }

        (bound, failure)
    }

    async fn start_https(
        paths: &Paths,
        routes: &Routes,
        settings: &GatewaySettings,
        launchd_installed: bool,
        activator: Arc<dyn Activator>,
        shutdown: Arc<Notify>,
    ) -> (Vec<SocketAddr>, Option<PathBuf>, Option<BindFailure>) {
        let ca = match LocalCa::load_or_create(&paths.ca_dir()) {
            Ok(ca) => Arc::new(ca),
            Err(err) => {
                tracing::warn!("no CA available, so HTTPS stays off: {err}");
                return (Vec::new(), None, Some(BindFailure::Other));
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
            return (addrs, Some(ca_path), None);
        }

        let (listeners, failure) = bind_with_fallback(
            settings,
            "the HTTPS proxy",
            settings.https_port,
            settings.fallback(
                settings.https_port_named,
                settings.https_port,
                FALLBACK_HTTPS_PORT,
                launchd_installed,
            ),
        )
        .await;

        let mut bound = Vec::with_capacity(listeners.len());

        for listener in listeners {
            bound.extend(listener.local_addr().ok());

            let routes = routes.clone();
            let activator = activator.clone();
            let shutdown = shutdown.clone();
            let tls = tls.clone();

            tokio::spawn(async move {
                if let Err(err) = serve_https(listener, routes, activator, tls, shutdown).await {
                    tracing::warn!("the HTTPS proxy stopped: {err}");
                }
            });
        }

        (bound, Some(ca_path), failure)
    }

    async fn start_dns(
        settings: &GatewaySettings,
        shutdown: Arc<Notify>,
    ) -> (Option<u16>, Option<BindFailure>) {
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
            return (port.or(Some(settings.dns_port)), None);
        }

        let addr = SocketAddr::new(settings.dns_bind, settings.dns_port);

        // Check the bind first. A failure inside serve() would leave the
        // caller unable to tell whether it started.
        match tokio::net::UdpSocket::bind(addr).await {
            Ok(socket) => drop(socket),
            Err(err) => {
                return (None, Some(report_bind_failure("DNS", addr, &err)));
            }
        }

        let config = DnsConfig::default();
        tokio::spawn(async move {
            if let Err(err) = minato_dns::serve(addr, config, shutdown).await {
                tracing::warn!("the DNS server stopped: {err}");
            }
        });

        tracing::info!("DNS: {addr}");
        (Some(settings.dns_port), None)
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
            http_failure: Some(BindFailure::Other),
            https_failure: Some(BindFailure::Other),
            dns_failure: Some(BindFailure::Other),
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
            http_failure: None,
            https_failure: None,
            dns_failure: None,
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

    pub fn http_failure(&self) -> Option<BindFailure> {
        self.http_failure
    }

    pub fn https_failure(&self) -> Option<BindFailure> {
        self.https_failure
    }

    pub fn dns_failure(&self) -> Option<BindFailure> {
        self.dns_failure
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

/// Binds `port` on every wanted address.
///
/// Partial success is kept rather than discarded: one family listening
/// still serves the clients that reach it, and [`Gateway::missing_families`]
/// is what says the other one is unattended.
async fn bind_port(
    bind: &[IpAddr],
    port: u16,
    what: &str,
) -> (Vec<TcpListener>, Option<BindFailure>) {
    let mut bound = Vec::new();
    let mut failure = None;

    for ip in bind {
        let addr = SocketAddr::new(*ip, port);

        match TcpListener::bind(addr).await {
            Ok(listener) => {
                tracing::info!("{what}: {addr}");
                bound.push(listener);
            }
            Err(err) => failure = Some(report_bind_failure(what, addr, &err)),
        }
    }

    (bound, failure)
}

/// Binds the wanted port, or the fallback when nothing at all came up.
///
/// **Only when nothing came up.** Moving after a partial bind would drop a
/// listener that is already serving, and the two families would end up on
/// different ports — one URL could not name both.
async fn bind_with_fallback(
    settings: &GatewaySettings,
    what: &str,
    port: u16,
    fallback: Option<u16>,
) -> (Vec<TcpListener>, Option<BindFailure>) {
    let (bound, failure) = bind_port(&settings.bind, port, what).await;

    if !bound.is_empty() {
        return (bound, failure);
    }

    let Some(fallback) = fallback else {
        return (bound, failure);
    };

    tracing::info!("{what}: {port} is out of reach, trying {fallback}");
    let (bound, fallback_failure) = bind_port(&settings.bind, fallback, what).await;

    // The first failure is the one worth reporting: it says why the port
    // anyone expects could not be had. Whatever went wrong on the fallback
    // only matters when that did not work either.
    if bound.is_empty() {
        return (bound, fallback_failure.or(failure));
    }

    (bound, None)
}

fn report_bind_failure(what: &str, addr: SocketAddr, err: &std::io::Error) -> BindFailure {
    let failure = BindFailure::classify(err, addr);

    match failure {
        BindFailure::Privileged => tracing::warn!(
            "{what} cannot hold {addr} (a port below 1024 needs privileges). \
             `minato doctor` says what to do about it"
        ),
        BindFailure::InUse => tracing::warn!(
            "{what} cannot hold {addr} (another process has it). \
             Check `minato doctor`"
        ),
        BindFailure::Other => tracing::warn!("{what} cannot hold {addr}: {err}"),
    }

    failure
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
    fn a_privileged_port_falls_back_to_a_high_one() {
        // Without this the proxy binds nothing, no URL is issued, and no
        // MINATO_URL_<SERVICE> reaches a container.
        let settings = GatewaySettings::default();

        assert_eq!(
            settings.fallback(false, settings.http_port, FALLBACK_HTTP_PORT, false),
            Some(FALLBACK_HTTP_PORT)
        );
    }

    #[test]
    fn a_named_port_is_never_moved_from() {
        // Setting MINATO_HTTP_PORT is how you decide this yourself.
        let settings = GatewaySettings::default();

        assert_eq!(
            settings.fallback(true, 8080, FALLBACK_HTTP_PORT, false),
            None
        );
    }

    #[test]
    fn the_fallback_does_not_fall_back_to_itself() {
        let settings = GatewaySettings::default();

        assert_eq!(
            settings.fallback(false, FALLBACK_HTTP_PORT, FALLBACK_HTTP_PORT, false),
            None
        );
    }

    #[test]
    fn launchd_holding_the_port_is_not_a_reason_to_move() {
        // launchd keeps 80 whether or not its job is running. Listening
        // elsewhere would leave the machine looking healthy while socket
        // activation stays broken, which is the bug #17 fixed.
        let settings = GatewaySettings::default();

        assert_eq!(
            settings.fallback(false, settings.http_port, FALLBACK_HTTP_PORT, true),
            None
        );
    }

    #[test]
    fn the_fallback_ports_are_unprivileged_and_out_of_the_way() {
        // A dev server on 8080 is common; taking it would be a worse
        // failure than the one being avoided.
        for port in [FALLBACK_HTTP_PORT, FALLBACK_HTTPS_PORT] {
            assert!(port >= 1024, "{port} would need privileges too");
            assert!(port > 10000, "{port} is in the range apps reach for");
        }
    }

    #[tokio::test]
    async fn a_taken_port_moves_to_the_fallback() {
        let settings = GatewaySettings {
            bind: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            ..GatewaySettings::default()
        };

        // Hold a port so the first attempt cannot have it.
        let taken = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .await
            .expect("binds");
        let port = taken.local_addr().expect("has an address").port();

        let (bound, failure) = bind_with_fallback(&settings, "test", port, Some(0)).await;

        assert_eq!(bound.len(), 1, "the fallback has to come up");
        assert!(failure.is_none(), "a fallback that worked is not a failure");
        assert_ne!(
            bound[0].local_addr().expect("bound").port(),
            port,
            "it must not be the port that was taken"
        );
    }

    #[tokio::test]
    async fn no_fallback_leaves_the_original_failure_showing() {
        let settings = GatewaySettings {
            bind: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            ..GatewaySettings::default()
        };

        let taken = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .await
            .expect("binds");
        let port = taken.local_addr().expect("has an address").port();

        let (bound, failure) = bind_with_fallback(&settings, "test", port, None).await;

        assert!(bound.is_empty());
        assert_eq!(failure, Some(BindFailure::InUse), "say why 80 was refused");
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
            http_failure: None,
            https_failure: None,
            dns_failure: None,
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
    fn a_taken_port_is_not_a_privileges_problem() {
        // launchd holds 80 while its job is idle, so this is what a bind
        // failure looks like after `minato daemon stop` — and it wants the
        // opposite advice from a permissions failure.
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 80);
        let in_use = std::io::Error::from(std::io::ErrorKind::AddrInUse);

        assert_eq!(BindFailure::classify(&in_use, addr), BindFailure::InUse);
    }

    #[test]
    fn a_privileged_port_is_only_privileged_below_1024() {
        let denied = || std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let low = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 80);
        let high = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080);

        assert_eq!(
            BindFailure::classify(&denied(), low),
            BindFailure::Privileged
        );
        assert_eq!(
            BindFailure::classify(&denied(), high),
            BindFailure::Other,
            "8080 being refused is not about privileges"
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
