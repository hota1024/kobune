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

    /// The port to try after `port`, if there is one worth naming.
    ///
    /// `None` when the port was asked for by name, or when it already *is*
    /// the fallback. Whether moving is right also depends on why the bind
    /// failed, which is not known yet here — see [`may_move`].
    fn fallback(&self, named: bool, port: u16, fallback: u16) -> Option<u16> {
        (!named && port != fallback).then_some(fallback)
    }
}

/// Whether a refused port is worth moving away from.
///
/// **Not when launchd is the one holding it.** A LaunchDaemon keeps 80
/// whether or not its job is running, so `InUse` there means the job needs
/// waking; listening elsewhere would leave the machine looking healthy
/// while socket activation stays broken.
///
/// A port that merely needs privileges is a different matter even with a
/// plist installed — nothing is holding it, so there is nothing to wake.
/// That is the state a `launchctl bootstrap` that never ran leaves behind,
/// and refusing to move there would issue no URLs at all.
///
/// `launchd_installed` is passed in rather than read here, so the rule can
/// be checked without a plist on the machine running the tests.
fn may_move(failure: Option<BindFailure>, launchd_installed: bool) -> bool {
    !(launchd_installed && failure == Some(BindFailure::InUse))
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
    /// Whether the proxy had to settle for the fallback port.
    ///
    /// Not the same as "the port is not 80": someone who names a port with
    /// `MINATO_HTTP_PORT` got what they asked for, and telling them that is
    /// unexpected would be wrong.
    http_fell_back: bool,
    https_fell_back: bool,
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

        let (http_addrs, http_failure, http_fell_back) = Self::start_http(
            &routes,
            settings,
            launchd_installed,
            activator.clone(),
            shutdown.clone(),
        )
        .await;
        let (https_addrs, ca_path, https_failure, https_fell_back) = Self::start_https(
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
            http_fell_back,
            https_fell_back,
        }
    }

    async fn start_http(
        routes: &Routes,
        settings: &GatewaySettings,
        launchd_installed: bool,
        activator: Arc<dyn Activator>,
        shutdown: Arc<Notify>,
    ) -> (Vec<SocketAddr>, Option<BindFailure>, bool) {
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
            return (addrs, None, false);
        }

        let listening = bind_with_fallback(
            settings,
            "the HTTP proxy",
            settings.http_port,
            settings.fallback(
                settings.http_port_named,
                settings.http_port,
                FALLBACK_HTTP_PORT,
            ),
            launchd_installed,
        )
        .await;

        let mut bound = Vec::with_capacity(listening.listeners.len());

        for listener in listening.listeners {
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

        (bound, listening.failure, listening.fell_back)
    }

    async fn start_https(
        paths: &Paths,
        routes: &Routes,
        settings: &GatewaySettings,
        launchd_installed: bool,
        activator: Arc<dyn Activator>,
        shutdown: Arc<Notify>,
    ) -> (Vec<SocketAddr>, Option<PathBuf>, Option<BindFailure>, bool) {
        let ca = match LocalCa::load_or_create(&paths.ca_dir()) {
            Ok(ca) => Arc::new(ca),
            Err(err) => {
                tracing::warn!("no CA available, so HTTPS stays off: {err}");
                return (Vec::new(), None, Some(BindFailure::Other), false);
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
            return (addrs, Some(ca_path), None, false);
        }

        let listening = bind_with_fallback(
            settings,
            "the HTTPS proxy",
            settings.https_port,
            settings.fallback(
                settings.https_port_named,
                settings.https_port,
                FALLBACK_HTTPS_PORT,
            ),
            launchd_installed,
        )
        .await;

        let mut bound = Vec::with_capacity(listening.listeners.len());

        for listener in listening.listeners {
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

        (bound, Some(ca_path), listening.failure, listening.fell_back)
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
                let failure = BindFailure::classify(&err, addr);
                report_bind_failure("DNS", addr.port(), Some(failure));
                return (None, Some(failure));
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
            http_fell_back: false,
            https_fell_back: false,
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
            http_fell_back: false,
            https_fell_back: false,
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

    /// The address families each proxy wanted and did not get.
    ///
    /// Anything here means requests to that address reach some other
    /// process. `*.localhost` resolves to both families and clients prefer
    /// IPv6, so the damage is real.
    ///
    /// **Per protocol.** They bind independently, and pooling them hides
    /// the case that matters: HTTPS losing `[::1]` while HTTP holds it
    /// leaves every check green and half the HTTPS traffic going to a
    /// stranger.
    pub fn missing_families(&self) -> Vec<(&'static str, IpAddr)> {
        let mut missing = Vec::new();

        for (protocol, bound) in [
            ("the HTTP proxy", &self.http_addrs),
            ("the HTTPS proxy", &self.https_addrs),
        ] {
            missing.extend(
                missing_from(bound, &self.wanted)
                    .into_iter()
                    .map(|family| (protocol, family)),
            );
        }

        missing
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

    pub fn http_fell_back(&self) -> bool {
        self.http_fell_back
    }

    pub fn https_fell_back(&self) -> bool {
        self.https_fell_back
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
async fn bind_port(bind: &[IpAddr], port: u16, what: &str) -> Attempt {
    let mut listeners = Vec::new();
    let mut failure = None;

    for ip in bind {
        let addr = SocketAddr::new(*ip, port);

        match TcpListener::bind(addr).await {
            Ok(listener) => {
                tracing::debug!("{what}: bound {addr}");
                listeners.push(listener);
            }
            Err(err) => {
                tracing::debug!("{what}: cannot hold {addr}: {err}");

                // The first refusal is kept rather than the last: it is the
                // reason the port could not be had, and a second family
                // failing differently does not change that.
                failure.get_or_insert_with(|| BindFailure::classify(&err, addr));
            }
        }
    }

    Attempt {
        port,
        listeners,
        failure,
    }
}

/// One port's worth of binding.
struct Attempt {
    port: u16,
    listeners: Vec<TcpListener>,
    failure: Option<BindFailure>,
}

impl Attempt {
    /// Whether every wanted address came up.
    fn is_complete(&self, wanted: usize) -> bool {
        self.listeners.len() == wanted
    }
}

/// What a listener ended up with.
pub struct Listening {
    pub listeners: Vec<TcpListener>,
    pub failure: Option<BindFailure>,
    /// Whether these are on the fallback rather than the wanted port.
    pub fell_back: bool,
}

/// Binds the wanted port, moving to the fallback when that does better.
///
/// **A complete bind on an awkward port beats a partial one on the right
/// port.** `*.localhost` resolves to `::1` and `127.0.0.1` alike, so a port
/// where only one family came up hands the other half of the traffic to
/// whatever else is listening — a URL that points at a stranger. Moving is
/// therefore weighed on how many addresses each port yields, not on whether
/// the first attempt got nothing.
///
/// The first attempt is held while the second is made, so nothing already
/// serving is given up before a replacement is in hand.
async fn bind_with_fallback(
    settings: &GatewaySettings,
    what: &str,
    port: u16,
    fallback: Option<u16>,
    launchd_installed: bool,
) -> Listening {
    let wanted = settings.bind.len();
    let attempt = bind_port(&settings.bind, port, what).await;

    if attempt.is_complete(wanted) {
        tracing::info!("{what}: :{port}");
        return Listening {
            listeners: attempt.listeners,
            failure: None,
            fell_back: false,
        };
    }

    let moved = match fallback.filter(|_| may_move(attempt.failure, launchd_installed)) {
        Some(fallback) => {
            tracing::info!("{what}: :{port} is not fully available, trying :{fallback}");
            bind_port(&settings.bind, fallback, what).await
        }
        None => return settled(what, attempt, false),
    };

    if moved.listeners.len() > attempt.listeners.len() {
        drop(attempt);
        return settled(what, moved, true);
    }

    // The fallback was no better, so keep what the wanted port gave and
    // report why that port fell short. Reporting the fallback's failure
    // instead would name a port nobody asked about.
    drop(moved);
    settled(what, attempt, false)
}

/// Reports the outcome once, at the level it deserves.
fn settled(what: &str, attempt: Attempt, fell_back: bool) -> Listening {
    let port = attempt.port;

    if attempt.listeners.is_empty() {
        report_bind_failure(what, port, attempt.failure);
    } else {
        tracing::info!("{what}: :{port} (some addresses could not be held)");
    }

    Listening {
        listeners: attempt.listeners,
        failure: attempt.failure,
        fell_back,
    }
}

/// The wanted families this listener did not get.
///
/// Empty when it bound nothing at all: that is `proxy-http` and
/// `proxy-https`'s business, and a listener that is entirely absent is not
/// a *family* gap.
fn missing_from(bound: &[SocketAddr], wanted: &[IpAddr]) -> Vec<IpAddr> {
    if bound.is_empty() {
        return Vec::new();
    }

    wanted
        .iter()
        .filter(|wanted| {
            !bound
                .iter()
                .any(|got| got.ip().is_ipv4() == wanted.is_ipv4())
        })
        .copied()
        .collect()
}

fn report_bind_failure(what: &str, port: u16, failure: Option<BindFailure>) {
    match failure {
        Some(BindFailure::Privileged) => tracing::warn!(
            "{what} cannot hold :{port} (a port below 1024 needs privileges). \
             `minato doctor` says what to do about it"
        ),
        Some(BindFailure::InUse) => tracing::warn!(
            "{what} cannot hold :{port} (another process has it). \
             Check `minato doctor`"
        ),
        _ => tracing::warn!("{what} cannot hold :{port}. Check `minato doctor`"),
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
    fn a_privileged_port_falls_back_to_a_high_one() {
        // Without this the proxy binds nothing, no URL is issued, and no
        // MINATO_URL_<SERVICE> reaches a container.
        let settings = GatewaySettings::default();

        assert_eq!(
            settings.fallback(false, settings.http_port, FALLBACK_HTTP_PORT),
            Some(FALLBACK_HTTP_PORT)
        );
    }

    #[test]
    fn a_named_port_is_never_moved_from() {
        // Setting MINATO_HTTP_PORT is how you decide this yourself.
        let settings = GatewaySettings::default();

        assert_eq!(settings.fallback(true, 8080, FALLBACK_HTTP_PORT), None);
    }

    #[test]
    fn the_fallback_does_not_fall_back_to_itself() {
        let settings = GatewaySettings::default();

        assert_eq!(
            settings.fallback(false, FALLBACK_HTTP_PORT, FALLBACK_HTTP_PORT),
            None
        );
    }

    #[test]
    fn launchd_holding_the_port_is_not_a_reason_to_move() {
        // launchd keeps 80 whether or not its job is running. Listening
        // elsewhere would leave the machine looking healthy while socket
        // activation stays broken, which is the bug #17 fixed.
        assert!(!may_move(Some(BindFailure::InUse), true));
    }

    #[test]
    fn a_plist_that_was_never_bootstrapped_still_moves() {
        // `launchctl bootstrap` failing leaves the plist on disk with
        // nothing holding 80. Refusing to move there would issue no URLs
        // at all — the very state this fallback exists to prevent.
        assert!(may_move(Some(BindFailure::Privileged), true));
    }

    #[test]
    fn without_launchd_every_refusal_is_worth_moving_from() {
        for failure in [
            Some(BindFailure::InUse),
            Some(BindFailure::Privileged),
            Some(BindFailure::Other),
            None,
        ] {
            assert!(may_move(failure, false), "{failure:?}");
        }
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

        let listening = bind_with_fallback(&settings, "test", port, Some(0), false).await;

        assert_eq!(listening.listeners.len(), 1, "the fallback has to come up");
        assert!(
            listening.failure.is_none(),
            "a fallback that worked is not a failure"
        );
        assert!(listening.fell_back, "and it has to say that it moved");
        assert_ne!(
            listening.listeners[0].local_addr().expect("bound").port(),
            port,
            "it must not be the port that was taken"
        );
    }

    #[tokio::test]
    async fn a_port_that_worked_is_not_reported_as_a_fallback() {
        let settings = GatewaySettings {
            bind: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            ..GatewaySettings::default()
        };

        let listening = bind_with_fallback(&settings, "test", 0, Some(0), false).await;

        assert_eq!(listening.listeners.len(), 1);
        assert!(!listening.fell_back);
    }

    #[tokio::test]
    async fn both_ports_failing_reports_why_the_wanted_one_did() {
        // Naming the fallback's problem would send someone to look at a
        // port they never asked about and bury the actionable cause. The
        // two have to fail *differently* for this to prove anything, so
        // the wanted port is taken (InUse) and the fallback is privileged.
        let settings = GatewaySettings {
            bind: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            ..GatewaySettings::default()
        };

        let wanted = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .await
            .expect("binds");
        let wanted_port = wanted.local_addr().expect("bound").port();

        // Port 1 needs privileges. Running as root it would simply bind,
        // which proves nothing either way.
        const PRIVILEGED: u16 = 1;
        if TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), PRIVILEGED))
            .await
            .is_ok()
        {
            return;
        }

        let listening =
            bind_with_fallback(&settings, "test", wanted_port, Some(PRIVILEGED), false).await;

        assert!(listening.listeners.is_empty());
        assert_eq!(
            listening.failure,
            Some(BindFailure::InUse),
            "the wanted port was taken; the fallback needing privileges is \
             not what anyone can act on"
        );
        assert!(!listening.fell_back, "nothing was fallen back to");
    }

    #[tokio::test]
    async fn a_complete_bind_beats_a_partial_one_on_the_wanted_port() {
        // *.localhost resolves to both families, so a port where only one
        // came up hands the other half of the traffic to a stranger.
        let settings = GatewaySettings::default();

        // Hold one family of the wanted port, leaving the other free.
        let half = TcpListener::bind(SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 0))
            .await
            .expect("binds");
        let wanted_port = half.local_addr().expect("bound").port();

        let listening = bind_with_fallback(&settings, "test", wanted_port, Some(0), false).await;

        assert_eq!(
            listening.listeners.len(),
            2,
            "it has to move to a port where both families come up"
        );
        assert!(listening.fell_back);
        assert!(listening.failure.is_none());
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

        let listening = bind_with_fallback(&settings, "test", port, None, false).await;

        assert!(listening.listeners.is_empty());
        assert_eq!(
            listening.failure,
            Some(BindFailure::InUse),
            "say why 80 was refused"
        );
        assert!(!listening.fell_back);
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

    /// A gateway with exactly these addresses bound, both families wanted.
    fn bound(http: Vec<SocketAddr>, https: Vec<SocketAddr>) -> Gateway {
        Gateway {
            routes: Routes::new(),
            http_addrs: http,
            https_addrs: https,
            dns_port: None,
            ca_path: None,
            wanted: vec![
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                IpAddr::V6(Ipv6Addr::LOCALHOST),
            ],
            http_failure: None,
            https_failure: None,
            dns_failure: None,
            http_fell_back: false,
            https_fell_back: false,
        }
    }

    fn v4(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    fn v6(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port)
    }

    #[test]
    fn detects_a_missing_address_family() {
        // With another app holding [::1]:8080, only IPv4 is available
        // here. *.localhost resolves to both, so anything arriving over
        // IPv6 goes to that app instead. This must not pass silently.
        let gateway = bound(vec![v4(8080)], Vec::new());

        assert_eq!(
            gateway.missing_families(),
            vec![("the HTTP proxy", IpAddr::V6(Ipv6Addr::LOCALHOST))]
        );
    }

    #[test]
    fn one_proxy_holding_a_family_does_not_cover_the_other() {
        // The case pooling the two addresses could not see: HTTP has both
        // families, HTTPS lost [::1] to something else. Clients prefer
        // IPv6, so half the HTTPS traffic goes to that process — while
        // every check reported green.
        let gateway = bound(vec![v4(80), v6(80)], vec![v4(443)]);

        assert_eq!(
            gateway.missing_families(),
            vec![("the HTTPS proxy", IpAddr::V6(Ipv6Addr::LOCALHOST))],
            "a family held for HTTP says nothing about HTTPS"
        );
    }

    #[test]
    fn each_proxy_is_reported_separately() {
        let gateway = bound(vec![v4(80)], vec![v6(443)]);
        let missing = gateway.missing_families();

        assert_eq!(missing.len(), 2, "{missing:?}");
        assert!(missing.contains(&("the HTTP proxy", IpAddr::V6(Ipv6Addr::LOCALHOST))));
        assert!(missing.contains(&("the HTTPS proxy", IpAddr::V4(Ipv4Addr::LOCALHOST))));
    }

    #[test]
    fn a_proxy_that_bound_nothing_is_not_a_family_gap() {
        // That is `proxy-http` and `proxy-https`'s business. Reporting it
        // here as well would say the same thing twice, in worse words.
        let gateway = bound(vec![v4(80), v6(80)], Vec::new());

        assert!(gateway.missing_families().is_empty(), "HTTPS is simply off");
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
