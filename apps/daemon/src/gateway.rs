//! 外から環境に届くための入り口。プロキシと DNS の待ち受けをまとめる。
//!
//! bind に失敗しても daemon は落とさない。80/443 は特権ポートで、
//! 権限が無い環境は珍しくない。その場合 URL を発行せず、`endpoint`
//! （ホストのポート直指定）だけを案内する方が、何も動かないより良い。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use minato_core::Paths;
use minato_dns::DnsConfig;
use minato_proxy::{LocalCa, Routes, serve_http, serve_https, server_config};
use tokio::net::TcpListener;
use tokio::sync::Notify;

/// 待ち受けポートを上書きする環境変数。
pub const HTTP_PORT_ENV: &str = "MINATO_HTTP_PORT";
pub const HTTPS_PORT_ENV: &str = "MINATO_HTTPS_PORT";
pub const DNS_PORT_ENV: &str = "MINATO_DNS_PORT";

/// DNS の既定ポート。
pub const DEFAULT_DNS_PORT: u16 = 53;

#[derive(Debug, Clone)]
pub struct GatewaySettings {
    pub http_port: u16,
    pub https_port: u16,
    pub dns_port: u16,
    /// プロキシの待ち受けアドレス。
    ///
    /// **IPv4 と IPv6 の両方のループバックで待ち受ける。** macOS は
    /// `*.localhost` を `::1` と `127.0.0.1` の両方に解決し、クライアントは
    /// IPv6 を優先する。片方しか押さえないと、`[::1]` にいる無関係の
    /// アプリへ silently 繋がってしまう。
    ///
    /// ローカル開発用なのでループバックに限る。0.0.0.0 にすると
    /// 同じ LAN の他人から開発環境が見えてしまう。
    pub bind: Vec<IpAddr>,
    /// DNS の待ち受けアドレス。resolver 設定が 127.0.0.1 を名指しするため
    /// 曖昧さがなく、IPv4 だけでよい。
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
    /// 環境変数で上書きする。特権ポートを避けたい場合に使う。
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
            tracing::warn!("{key} の値 `{raw}` はポート番号として解釈できません");
            None
        }
    }
}

/// 起動済みの入り口。
pub struct Gateway {
    routes: Routes,
    /// 実際に bind できたポート。失敗したものは `None`。
    http_port: Option<u16>,
    https_port: Option<u16>,
    dns_port: Option<u16>,
    ca_path: Option<PathBuf>,
}

impl Gateway {
    /// プロキシと DNS を起動する。失敗したものは無効のまま先へ進む。
    pub async fn start(paths: &Paths, settings: &GatewaySettings, shutdown: Arc<Notify>) -> Self {
        let routes = Routes::new();

        let http_port = Self::start_http(&routes, settings, shutdown.clone()).await;
        let (https_port, ca_path) =
            Self::start_https(paths, &routes, settings, shutdown.clone()).await;
        let dns_port = Self::start_dns(settings, shutdown).await;

        Self {
            routes,
            http_port,
            https_port,
            dns_port,
            ca_path,
        }
    }

    async fn start_http(
        routes: &Routes,
        settings: &GatewaySettings,
        shutdown: Arc<Notify>,
    ) -> Option<u16> {
        let mut bound = None;

        for ip in &settings.bind {
            let addr = SocketAddr::new(*ip, settings.http_port);

            match TcpListener::bind(addr).await {
                Ok(listener) => {
                    bound = Some(
                        listener
                            .local_addr()
                            .map(|a| a.port())
                            .unwrap_or(settings.http_port),
                    );

                    let routes = routes.clone();
                    let shutdown = shutdown.clone();

                    tokio::spawn(async move {
                        if let Err(err) = serve_http(listener, routes, shutdown).await {
                            tracing::warn!("HTTP プロキシが停止しました: {err}");
                        }
                    });

                    tracing::info!("HTTP プロキシ: {addr}");
                }
                Err(err) => report_bind_failure("HTTP プロキシ", addr, &err),
            }
        }

        bound
    }

    async fn start_https(
        paths: &Paths,
        routes: &Routes,
        settings: &GatewaySettings,
        shutdown: Arc<Notify>,
    ) -> (Option<u16>, Option<PathBuf>) {
        let ca = match LocalCa::load_or_create(&paths.ca_dir()) {
            Ok(ca) => Arc::new(ca),
            Err(err) => {
                tracing::warn!("CA を用意できないため HTTPS を無効にします: {err}");
                return (None, None);
            }
        };

        let ca_path = ca.certificate_path();
        let tls = server_config(ca);
        let mut bound = None;

        for ip in &settings.bind {
            let addr = SocketAddr::new(*ip, settings.https_port);

            match TcpListener::bind(addr).await {
                Ok(listener) => {
                    bound = Some(
                        listener
                            .local_addr()
                            .map(|a| a.port())
                            .unwrap_or(settings.https_port),
                    );

                    let routes = routes.clone();
                    let shutdown = shutdown.clone();
                    let tls = tls.clone();

                    tokio::spawn(async move {
                        if let Err(err) = serve_https(listener, routes, tls, shutdown).await {
                            tracing::warn!("HTTPS プロキシが停止しました: {err}");
                        }
                    });

                    tracing::info!("HTTPS プロキシ: {addr}");
                }
                Err(err) => report_bind_failure("HTTPS プロキシ", addr, &err),
            }
        }

        (bound, Some(ca_path))
    }

    async fn start_dns(settings: &GatewaySettings, shutdown: Arc<Notify>) -> Option<u16> {
        let addr = SocketAddr::new(settings.dns_bind, settings.dns_port);

        // 先に bind できるか確かめる。serve() の中で失敗すると
        // 起動したのかどうかが呼び出し側から分からない。
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
                tracing::warn!("DNS サーバが停止しました: {err}");
            }
        });

        tracing::info!("DNS: {addr}");
        Some(settings.dns_port)
    }

    /// 何も待ち受けていない入り口。bind に全部失敗した状況と同じ。
    #[cfg(test)]
    pub(crate) fn inert() -> Self {
        Self {
            routes: Routes::new(),
            http_port: None,
            https_port: None,
            dns_port: None,
            ca_path: None,
        }
    }

    /// ポートを指定した入り口。テストで URL の組み立てを確かめるのに使う。
    #[cfg(test)]
    pub(crate) fn with_ports(http: Option<u16>, https: Option<u16>) -> Self {
        Self {
            routes: Routes::new(),
            http_port: http,
            https_port: https,
            dns_port: None,
            ca_path: None,
        }
    }

    pub fn routes(&self) -> &Routes {
        &self.routes
    }

    pub fn http_port(&self) -> Option<u16> {
        self.http_port
    }

    pub fn https_port(&self) -> Option<u16> {
        self.https_port
    }

    pub fn dns_port(&self) -> Option<u16> {
        self.dns_port
    }

    pub fn ca_path(&self) -> Option<&std::path::Path> {
        self.ca_path.as_deref()
    }

    /// プロキシが動いているか。動いていなければ URL を発行しない。
    pub fn is_serving(&self) -> bool {
        self.http_port.is_some() || self.https_port.is_some()
    }

    /// ホスト名に対応する URL。プロキシが動いていなければ `None`。
    ///
    /// HTTPS を優先する。ブラウザは HTTP の開発サーバに対しても
    /// 混在コンテンツや Secure Cookie の制約をかけるため。
    pub fn url_for(&self, host: &str) -> Option<String> {
        if let Some(port) = self.https_port {
            return Some(format_url("https", host, port, 443));
        }

        self.http_port
            .map(|port| format_url("http", host, port, 80))
    }
}

/// 既定ポートなら省略した URL を作る。
fn format_url(scheme: &str, host: &str, port: u16, default_port: u16) -> String {
    if port == default_port {
        format!("{scheme}://{host}")
    } else {
        format!("{scheme}://{host}:{port}")
    }
}

fn report_bind_failure(what: &str, addr: SocketAddr, err: &std::io::Error) {
    if err.kind() == std::io::ErrorKind::PermissionDenied && addr.port() < 1024 {
        tracing::warn!(
            "{what} が {addr} を確保できません（1024 未満のポートには権限が要ります）。\
             `minato doctor` に対処方法があります"
        );
    } else if err.kind() == std::io::ErrorKind::AddrInUse {
        tracing::warn!(
            "{what} が {addr} を確保できません（他のプロセスが使用中）。\
             `minato doctor` で確認してください"
        );
    } else {
        tracing::warn!("{what} が {addr} を確保できません: {err}");
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
        // 待ち受けていないのに URL を出すと、繋がらない先を案内することになる。
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
            "0.0.0.0 にすると LAN から開発環境が見えてしまう"
        );
    }

    #[test]
    fn listens_on_both_loopback_families() {
        // *.localhost は ::1 と 127.0.0.1 の両方に解決される。片方しか
        // 押さえないと、もう片方にいる別のアプリへ繋がってしまう。
        let settings = GatewaySettings::default();

        assert!(settings.bind.contains(&IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(settings.bind.contains(&IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(
            settings.bind.iter().all(|ip| ip.is_loopback()),
            "ループバック以外に晒さない"
        );
    }
}
