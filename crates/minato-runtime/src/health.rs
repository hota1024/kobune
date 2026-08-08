//! Deciding whether a service is ready to serve.
//!
//! A container having started and the app inside being able to answer are
//! two different things. Confusing them makes the `curl` right after
//! `minato up` fail with connection refused, and an agent reads that as
//! "the server is broken".
//!
//! Follows `health` from `minato.toml` when it is set; otherwise all that
//! matters is whether a TCP connection can be made.

use std::net::SocketAddr;
use std::time::Duration;

use minato_core::HealthCheck;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::error::{Result, RuntimeError};
use crate::event::EventSink;

/// How long to wait for readiness by default.
///
/// A dev server's first start — resolving dependencies, compiling — can
/// take longer than this. When it does, carry on and warn rather than
/// wait: waiting forever means `minato up` never returns.
pub const DEFAULT_READINESS_TIMEOUT: Duration = Duration::from_secs(15);

/// How often to check.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How long a single check may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Checks once.
///
/// A `health` of `None` means "can a TCP connection be made".
pub async fn probe(endpoint: SocketAddr, health: Option<&HealthCheck>) -> Result<bool> {
    match health {
        None => Ok(probe_tcp(endpoint).await),
        Some(HealthCheck::Tcp(_)) => Ok(probe_tcp(endpoint).await),
        Some(HealthCheck::Http(url)) => Ok(probe_http(endpoint, url).await),
        Some(HealthCheck::Cmd(command)) => Err(RuntimeError::Unsupported(format!(
            "health = \"cmd:{command}\" is not supported yet. \
             Use `http://...` or `tcp://...`"
        ))),
    }
}

/// Whether a TCP connection can be made.
async fn probe_tcp(endpoint: SocketAddr) -> bool {
    tokio::time::timeout(PROBE_TIMEOUT, tokio::net::TcpStream::connect(endpoint))
        .await
        .is_ok_and(|result| result.is_ok())
}

/// Whether an HTTP request comes back 2xx or 3xx.
///
/// The host in the `health` URL is ignored: **only its path is used, and
/// the request goes to `endpoint`**. The configuration is written from
/// inside the container (`http://localhost:3000/healthz`), but what the
/// host can reach is whatever address the runtime assigned.
async fn probe_http(endpoint: SocketAddr, url: &str) -> bool {
    let path = path_of(url);

    let Ok(Ok(mut stream)) =
        tokio::time::timeout(PROBE_TIMEOUT, tokio::net::TcpStream::connect(endpoint)).await
    else {
        return false;
    };

    // A minimal HTTP/1.1 request, written by hand to avoid another
    // dependency. A health check only needs the status line.
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {endpoint}\r\nConnection: close\r\nUser-Agent: minato-health\r\n\r\n"
    );

    if tokio::time::timeout(PROBE_TIMEOUT, stream.write_all(request.as_bytes()))
        .await
        .is_err()
    {
        return false;
    }

    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();

    if tokio::time::timeout(PROBE_TIMEOUT, reader.read_line(&mut status_line))
        .await
        .is_err()
    {
        return false;
    }

    matches!(status_code(&status_line), Some(code) if (200..400).contains(&code))
}

/// Takes `/healthz?x=1` out of `http://localhost:3000/healthz?x=1`.
fn path_of(url: &str) -> String {
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);

    match without_scheme.find('/') {
        Some(index) => without_scheme[index..].to_string(),
        None => "/".to_string(),
    }
}

/// Takes `200` out of `HTTP/1.1 200 OK`.
fn status_code(status_line: &str) -> Option<u16> {
    status_line.split_whitespace().nth(1)?.parse().ok()
}

/// Waits for readiness.
///
/// Running out of time is not itself a failure — the app may simply be
/// slow to start. Returns whether it became ready.
pub async fn wait_until_ready(
    endpoint: SocketAddr,
    health: Option<&HealthCheck>,
    timeout: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        match probe(endpoint, health).await {
            Ok(true) => return true,
            // An unsupported check will not start working if we wait.
            Err(_) => return false,
            Ok(false) => {}
        }

        if tokio::time::Instant::now() >= deadline {
            return false;
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Waits for a service to answer, warning if it does not.
pub async fn await_service(
    service: &str,
    endpoint: Option<SocketAddr>,
    health: Option<&HealthCheck>,
    timeout: Duration,
    events: &EventSink,
) -> bool {
    let Some(addr) = endpoint else {
        // A service that publishes no port cannot be connected to at all.
        return true;
    };

    let label = format!("waiting for {service}");
    events.step_started("await", &label);

    if wait_until_ready(addr, health, timeout).await {
        events.step_done("await", &label);
        return true;
    }

    events.step_skipped(
        "await",
        &label,
        format!("no answer within {} seconds", timeout.as_secs()),
    );
    events.warn(format!(
        "{service} is not answering on {addr} yet. \
         It may just be slow to start"
    ));

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn extracts_the_path_from_a_health_url() {
        // The configuration is written from inside the container. What
        // the host can reach is a different address, so only the path is
        // used.
        assert_eq!(path_of("http://localhost:3000/healthz"), "/healthz");
        assert_eq!(path_of("https://localhost/ready?deep=1"), "/ready?deep=1");
        assert_eq!(path_of("http://localhost:3000"), "/");
        assert_eq!(path_of("http://127.0.0.1"), "/");
    }

    #[test]
    fn parses_status_codes() {
        assert_eq!(status_code("HTTP/1.1 200 OK"), Some(200));
        assert_eq!(status_code("HTTP/1.1 503 Service Unavailable"), Some(503));
        assert_eq!(status_code("garbage"), None);
        assert_eq!(status_code(""), None);
    }

    /// An HTTP server that returns one fixed status line.
    async fn spawn_http(status: &'static str) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");

        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let response =
                    format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });

        addr
    }

    async fn closed_port() -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);
        addr
    }

    #[tokio::test]
    async fn tcp_probe_follows_the_listener() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");

        assert!(probe(addr, None).await.expect("can decide"));

        drop(listener);
        assert!(!probe(closed_port().await, None).await.expect("can decide"));
    }

    #[tokio::test]
    async fn http_probe_accepts_success_and_redirects() {
        for status in ["200 OK", "204 No Content", "302 Found"] {
            let addr = spawn_http(status).await;
            let health = HealthCheck::Http("http://localhost/healthz".into());

            assert!(
                probe(addr, Some(&health)).await.expect("can decide"),
                "{status} counts as ready"
            );
        }
    }

    #[tokio::test]
    async fn http_probe_rejects_server_errors() {
        // Rejects "started, but not connected to its dependencies".
        let addr = spawn_http("503 Service Unavailable").await;
        let health = HealthCheck::Http("http://localhost/healthz".into());

        assert!(!probe(addr, Some(&health)).await.expect("can decide"));
    }

    #[tokio::test]
    async fn http_probe_fails_when_nothing_listens() {
        let addr = closed_port().await;
        let health = HealthCheck::Http("http://localhost/healthz".into());

        assert!(!probe(addr, Some(&health)).await.expect("can decide"));
    }

    #[tokio::test]
    async fn tcp_health_only_needs_a_connection() {
        // With a TCP check, a service that speaks no HTTP is still ready.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let health = HealthCheck::Tcp("localhost:5432".into());

        assert!(probe(addr, Some(&health)).await.expect("can decide"));
    }

    #[tokio::test]
    async fn cmd_health_reports_that_it_is_unsupported() {
        let addr = closed_port().await;
        let health = HealthCheck::Cmd("pg_isready".into());

        let err = probe(addr, Some(&health)).await.unwrap_err();
        assert!(err.to_string().contains("not supported"), "got: {err}");
    }

    #[tokio::test]
    async fn waiting_gives_up_on_unsupported_checks() {
        // Do not keep waiting on something waiting cannot fix.
        let addr = closed_port().await;
        let health = HealthCheck::Cmd("pg_isready".into());

        let started = std::time::Instant::now();
        let ready = wait_until_ready(addr, Some(&health), Duration::from_secs(5)).await;

        assert!(!ready);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "it has to give up immediately"
        );
    }

    #[tokio::test]
    async fn waits_for_a_service_that_becomes_ready_late() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let late = tokio::net::TcpListener::bind(addr).await.expect("rebinds");
            tokio::time::sleep(Duration::from_secs(5)).await;
            drop(late);
        });

        assert!(wait_until_ready(addr, None, Duration::from_secs(3)).await);
    }

    #[tokio::test]
    async fn services_without_a_port_are_considered_ready() {
        let (sink, mut rx) = EventSink::channel();

        assert!(await_service("db", None, None, Duration::from_millis(100), &sink).await);
        drop(sink);

        assert!(rx.recv().await.is_none(), "nothing to wait for, so nothing to report");
    }

    #[tokio::test]
    async fn warns_but_does_not_fail_when_the_service_is_slow() {
        let addr = closed_port().await;
        let (sink, mut rx) = EventSink::channel();

        let ready = await_service("web", Some(addr), None, Duration::from_millis(200), &sink).await;
        drop(sink);

        assert!(!ready, "the caller is told it never answered");

        let mut saw_warning = false;
        while let Some(event) = rx.recv().await {
            if let minato_api::Event::Log {
                level: minato_api::LogLevel::Warn,
                ..
            } = event
            {
                saw_warning = true;
            }
        }

        assert!(saw_warning, "the user has to be told what happened");
    }
}
