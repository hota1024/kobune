//! Deciding whether a service is ready to serve.
//!
//! A container having started and the app inside being able to answer are
//! two different things. Confusing them makes the `curl` right after
//! `kobune up` fail with connection refused, and an agent reads that as
//! "the server is broken".
//!
//! Follows `health` from `kobune.toml` when it is set; otherwise all that
//! matters is whether the endpoint holds a connection open.

use std::net::SocketAddr;
use std::time::Duration;

use kobune_core::HealthCheck;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::error::{Result, RuntimeError};
use crate::event::EventSink;

/// How long to wait for readiness by default.
///
/// A dev server's first start — resolving dependencies, compiling — can
/// take longer than this. When it does, carry on and warn rather than
/// wait: waiting forever means `kobune up` never returns.
pub const DEFAULT_READINESS_TIMEOUT: Duration = Duration::from_secs(15);

/// How often to check.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How long a single check may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// How long a connection has to stay open to count as an app answering.
///
/// See [`probe_tcp`]. Measured against Docker, the hang-up arrives within
/// about a millisecond and a real dev server holds the connection open
/// indefinitely, so anything in between works. This is the readiness
/// check's whole cost once a service is up, which is why it is not larger.
const SETTLE: Duration = Duration::from_millis(100);

/// Runs a health command inside the service's container.
///
/// Narrower than [`crate::Runtime::exec`] on purpose: that one streams the
/// output as events, and a check running every 100ms would bury everything
/// else. Only the exit status matters here.
#[async_trait::async_trait]
pub trait CommandProbe: Send + Sync {
    /// Whether the command exited 0.
    ///
    /// Anything that goes wrong — the container is gone, the binary is not
    /// in the image — reads as "not ready yet". A check that has not
    /// succeeded is the same to the caller however it failed, and the
    /// timeout is what turns a permanent failure into a warning.
    async fn succeeds(&self, command: &[String]) -> bool;
}

/// Checks once.
///
/// A `health` of `None` means [`probe_tcp`] — is anything listening.
///
/// `exec` is what runs a `cmd:` check. Without one — nothing to run it
/// through — such a check is reported as unsupported rather than silently
/// passing.
pub async fn probe(
    endpoint: SocketAddr,
    health: Option<&HealthCheck>,
    exec: Option<&dyn CommandProbe>,
) -> Result<bool> {
    match health {
        None => Ok(probe_tcp(endpoint).await),
        Some(HealthCheck::Tcp(_)) => Ok(probe_tcp(endpoint).await),
        Some(HealthCheck::Http(url)) => Ok(probe_http(endpoint, url).await),
        Some(HealthCheck::Cmd(command)) => match exec {
            Some(exec) => Ok(probe_cmd(command, exec).await),
            None => Err(RuntimeError::Unsupported(format!(
                "health = \"cmd:{command}\" cannot run here. \
                 Use `http://...` or `tcp://...`"
            ))),
        },
    }
}

/// Whether the health command succeeds inside the container.
///
/// The command is split shell-style, so `cmd:pg_isready -U postgres` works
/// as written. It runs without a shell, so pipes and redirections do not —
/// wrap them in `sh -c` if you need them.
async fn probe_cmd(command: &str, exec: &dyn CommandProbe) -> bool {
    let Ok(argv) = shell_words::split(command) else {
        return false;
    };

    if argv.is_empty() {
        return false;
    }

    tokio::time::timeout(PROBE_TIMEOUT, exec.succeeds(&argv))
        .await
        .unwrap_or(false)
}

/// Whether the endpoint belongs to something that is actually listening.
///
/// **Connecting is not enough.** A published Docker port is held by the
/// runtime, not by the app: the connection is accepted and then closed
/// again the moment the dial into the container fails. A probe that only
/// connects therefore passes the instant the container exists, the service
/// is called ready before the dev server has bound its port, and the first
/// request through the proxy comes back as `connection closed before
/// message completed`.
///
/// So the connection is held for a moment instead. Nothing is sent — which
/// keeps this usable for a service that speaks no HTTP — and the peer
/// hanging up unprompted is what says nobody is home.
async fn probe_tcp(endpoint: SocketAddr) -> bool {
    let Ok(Ok(stream)) =
        tokio::time::timeout(PROBE_TIMEOUT, tokio::net::TcpStream::connect(endpoint)).await
    else {
        return false;
    };

    // Peeked rather than read: a server that opens with a banner keeps it
    // for whoever connects next.
    let mut first = [0u8; 1];

    match tokio::time::timeout(SETTLE, stream.peek(&mut first)).await {
        // Still open with nothing to say, which is how a server waiting
        // for a request behaves.
        Err(_) => true,
        // Hung up on us, or reset the connection.
        Ok(Ok(0)) | Ok(Err(_)) => false,
        // Spoke first (SSH, SMTP, a database greeting).
        Ok(Ok(_)) => true,
    }
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
        "GET {path} HTTP/1.1\r\nHost: {endpoint}\r\nConnection: close\r\nUser-Agent: kobune-health\r\n\r\n"
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
    exec: Option<&dyn CommandProbe>,
    timeout: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        match probe(endpoint, health, exec).await {
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
    exec: Option<&dyn CommandProbe>,
    timeout: Duration,
    events: &EventSink,
) -> bool {
    let Some(addr) = endpoint else {
        // A service that publishes no port cannot be connected to at all.
        // A `cmd:` check does not need one, though, so it still runs.
        return match health {
            Some(HealthCheck::Cmd(_)) => {
                await_command_only(service, health, exec, timeout, events).await
            }
            _ => true,
        };
    };

    // The id says only what this is, not which service it is for: the
    // caller has scoped the sink (`EventSink::for_service`), so several of
    // these can run at once without colliding.
    let label = format!("waiting for {service}");
    events.step_started("await", &label);

    if wait_until_ready(addr, health, exec, timeout).await {
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

/// Waits on a `cmd:` check for a service that publishes no port.
///
/// The usual readiness path needs an address to connect to. A command runs
/// inside the container and needs none, which is exactly the case for a
/// database that is reached only by other services.
async fn await_command_only(
    service: &str,
    health: Option<&HealthCheck>,
    exec: Option<&dyn CommandProbe>,
    timeout: Duration,
    events: &EventSink,
) -> bool {
    let label = format!("waiting for {service}");
    events.step_started("await", &label);

    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        // A dummy address: `probe` does not touch it on the `cmd:` path.
        let unused = SocketAddr::from(([127, 0, 0, 1], 0));

        match probe(unused, health, exec).await {
            Ok(true) => {
                events.step_done("await", &label);
                return true;
            }
            Err(_) => {
                events.step_skipped("await", &label, "the check cannot run here");
                return false;
            }
            Ok(false) => {}
        }

        if tokio::time::Instant::now() >= deadline {
            events.step_skipped(
                "await",
                &label,
                format!("no answer within {} seconds", timeout.as_secs()),
            );
            return false;
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    }
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

    /// Accepts and hangs up without a word, the way a published Docker
    /// port behaves while the container has nothing behind it.
    async fn spawn_accept_then_close() -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");

        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                drop(stream);
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

        assert!(probe(addr, None, None).await.expect("can decide"));

        drop(listener);
        assert!(
            !probe(closed_port().await, None, None)
                .await
                .expect("can decide")
        );
    }

    #[tokio::test]
    async fn a_connection_that_is_dropped_at_once_is_not_ready() {
        // The bug this exists for: Docker accepts on a published port
        // whether or not anything inside the container is listening, so a
        // probe that stops at "connected" calls every container ready the
        // moment it is created.
        assert!(
            !probe(spawn_accept_then_close().await, None, None)
                .await
                .expect("can decide")
        );

        // Explicit `tcp:` is the same check and has the same problem.
        let health = HealthCheck::Tcp("localhost:5432".into());
        assert!(
            !probe(spawn_accept_then_close().await, Some(&health), None)
                .await
                .expect("can decide")
        );
    }

    #[tokio::test]
    async fn a_server_that_speaks_first_is_ready() {
        // Not everything waits to be asked. A greeting is as good an
        // answer as staying open, and it is peeked so the next connection
        // still gets it.
        let addr = spawn_http("200 OK").await;
        assert!(probe(addr, None, None).await.expect("can decide"));
    }

    #[tokio::test]
    async fn waiting_is_bounded_by_the_settle_time() {
        // The check runs on every poll of a service that is already up,
        // so it must not turn readiness into a long wait.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");

        let started = std::time::Instant::now();
        assert!(probe(addr, None, None).await.expect("can decide"));
        assert!(
            started.elapsed() < SETTLE * 3,
            "took {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn http_probe_accepts_success_and_redirects() {
        for status in ["200 OK", "204 No Content", "302 Found"] {
            let addr = spawn_http(status).await;
            let health = HealthCheck::Http("http://localhost/healthz".into());

            assert!(
                probe(addr, Some(&health), None).await.expect("can decide"),
                "{status} counts as ready"
            );
        }
    }

    #[tokio::test]
    async fn http_probe_rejects_server_errors() {
        // Rejects "started, but not connected to its dependencies".
        let addr = spawn_http("503 Service Unavailable").await;
        let health = HealthCheck::Http("http://localhost/healthz".into());

        assert!(!probe(addr, Some(&health), None).await.expect("can decide"));
    }

    #[tokio::test]
    async fn http_probe_fails_when_nothing_listens() {
        let addr = closed_port().await;
        let health = HealthCheck::Http("http://localhost/healthz".into());

        assert!(!probe(addr, Some(&health), None).await.expect("can decide"));
    }

    #[tokio::test]
    async fn tcp_health_does_not_need_http() {
        // With a TCP check, a service that speaks no HTTP is still ready.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let health = HealthCheck::Tcp("localhost:5432".into());

        assert!(probe(addr, Some(&health), None).await.expect("can decide"));
    }

    #[tokio::test]
    async fn a_cmd_check_without_anywhere_to_run_it_is_unsupported() {
        // Reachable only through a runtime. Reporting it as unsupported
        // beats passing a check that never ran.
        let addr = closed_port().await;
        let health = HealthCheck::Cmd("pg_isready".into());

        let err = probe(addr, Some(&health), None).await.unwrap_err();
        assert!(err.to_string().contains("cannot run here"), "got: {err}");
    }

    /// Reports whatever it was told to.
    struct Scripted {
        succeeds: bool,
        calls: std::sync::Mutex<Vec<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl CommandProbe for Scripted {
        async fn succeeds(&self, command: &[String]) -> bool {
            self.calls.lock().expect("lock").push(command.to_vec());
            self.succeeds
        }
    }

    fn scripted(succeeds: bool) -> Scripted {
        Scripted {
            succeeds,
            calls: std::sync::Mutex::new(Vec::new()),
        }
    }

    #[tokio::test]
    async fn a_cmd_check_passes_when_the_command_does() {
        let addr = closed_port().await;
        let health = HealthCheck::Cmd("pg_isready -U postgres".into());
        let exec = scripted(true);

        assert!(probe(addr, Some(&health), Some(&exec)).await.expect("runs"));

        // Split shell-style, so arguments arrive as arguments rather than
        // one string the container would try to exec as a filename.
        let calls = exec.calls.lock().expect("lock").clone();
        assert_eq!(calls, vec![vec!["pg_isready", "-U", "postgres"]]);
    }

    #[tokio::test]
    async fn a_failing_command_is_not_ready() {
        // A database that accepts TCP before it accepts queries is the
        // reason cmd: exists, so a non-zero exit has to mean "keep waiting".
        let addr = closed_port().await;
        let health = HealthCheck::Cmd("pg_isready".into());

        assert!(
            !probe(addr, Some(&health), Some(&scripted(false)))
                .await
                .expect("runs")
        );
    }

    #[tokio::test]
    async fn a_cmd_check_does_not_need_a_port() {
        // The case it is for: a database other services reach directly,
        // with expose = false and nothing for the host to connect to.
        let health = HealthCheck::Cmd("pg_isready".into());
        let (sink, _rx) = EventSink::channel();

        assert!(
            await_service(
                "db",
                None,
                Some(&health),
                Some(&scripted(true)),
                Duration::from_millis(500),
                &sink,
            )
            .await
        );
    }

    #[tokio::test]
    async fn an_unparseable_command_is_not_ready() {
        let addr = closed_port().await;
        let health = HealthCheck::Cmd("pg_isready \"unclosed".into());

        assert!(
            !probe(addr, Some(&health), Some(&scripted(true)))
                .await
                .expect("runs")
        );
    }

    #[tokio::test]
    async fn waiting_gives_up_on_unsupported_checks() {
        // Do not keep waiting on something waiting cannot fix.
        let addr = closed_port().await;
        let health = HealthCheck::Cmd("pg_isready".into());

        let started = std::time::Instant::now();
        let ready = wait_until_ready(addr, Some(&health), None, Duration::from_secs(5)).await;

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

        assert!(wait_until_ready(addr, None, None, Duration::from_secs(3)).await);
    }

    #[tokio::test]
    async fn services_without_a_port_are_considered_ready() {
        let (sink, mut rx) = EventSink::channel();

        assert!(await_service("db", None, None, None, Duration::from_millis(100), &sink).await);
        drop(sink);

        assert!(
            rx.recv().await.is_none(),
            "nothing to wait for, so nothing to report"
        );
    }

    #[tokio::test]
    async fn warns_but_does_not_fail_when_the_service_is_slow() {
        let addr = closed_port().await;
        let (sink, mut rx) = EventSink::channel();

        let ready = await_service(
            "web",
            Some(addr),
            None,
            None,
            Duration::from_millis(200),
            &sink,
        )
        .await;
        drop(sink);

        assert!(!ready, "the caller is told it never answered");

        let mut saw_warning = false;
        while let Some(event) = rx.recv().await {
            if let kobune_api::Event::Log {
                level: kobune_api::LogLevel::Warn,
                ..
            } = event
            {
                saw_warning = true;
            }
        }

        assert!(saw_warning, "the user has to be told what happened");
    }
}
