//! Driving the `cloudflared` CLI, and owning the process that runs.
//!
//! Setup goes through the CLI rather than Cloudflare's HTTP API so there
//! is no API token to obtain, store or scope. `cloudflared tunnel login`
//! already leaves a certificate behind, and `create` and `route dns` use
//! it. The one thing the CLI cannot do is apply an Access policy, which is
//! why exposing a tunnel without one has to be asked for explicitly.

use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::process::{Child, Command};

use crate::provider::{Hostnames, RunningTunnel, TunnelRequest};
use crate::{Result, TunnelError};

use super::config;

/// How long a setup command may take.
///
/// These reach Cloudflare's API, so they are not instant, but any of them
/// still sitting here after this is not going to finish.
const SETUP_TIMEOUT: Duration = Duration::from_secs(30);

/// What `cloudflared` says when the work is already done.
///
/// Both setup steps are run on every enable, so "it exists" has to read as
/// success — otherwise re-running `kobune tunnel enable` fails on a
/// machine where it already worked.
const ALREADY_EXISTS: &[&str] = &[
    "already exists",
    "already configured",
    "record with that host already exists",
];

/// A running tunnel.
///
/// Dropping it kills the child, which is what should happen: a tunnel
/// outliving the daemon would keep publishing an environment nothing is
/// managing any more. That takes [`Command::kill_on_drop`] below —
/// tokio's default is to leave the process running and merely reap it,
/// so the promise is not free.
#[derive(Debug)]
pub struct TunnelProcess {
    child: Child,
    hostnames: Hostnames,
}

#[async_trait]
impl RunningTunnel for TunnelProcess {
    /// The zone, which reaches here the moment DNS is routed.
    ///
    /// Known before the process starts rather than learned from it: the
    /// wildcard record is what makes a name arrive, and it was written by
    /// the step above. Nothing cloudflared prints would add to it.
    fn hostnames(&self) -> &Hostnames {
        &self.hostnames
    }

    /// Whether the child is still alive.
    ///
    /// cloudflared exiting on its own — a revoked credential, a deleted
    /// tunnel — otherwise goes unnoticed, and every tunnel URL quietly
    /// stops working while `status` still claims it is up.
    fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    async fn stop(mut self: Box<Self>) {
        if let Err(err) = self.child.kill().await {
            tracing::debug!("cannot stop cloudflared: {err}");
        }
    }
}

/// Prepares the tunnel and starts it.
///
/// Creating the tunnel and routing DNS are both idempotent, so this is
/// safe to call on every daemon start rather than only the first.
///
/// The DNS outcome comes back with the process because the caller is the
/// only one that knows whether Kobune has routed this zone before, and so
/// whether "already existed" is its own record or a stranger's.
pub async fn start(
    program: &str,
    request: &TunnelRequest,
    domain: &str,
) -> Result<(TunnelProcess, StepOutcome)> {
    ensure_tunnel(program, request).await?;
    let dns = ensure_dns(program, request, domain).await?;

    let path = config::write_config(request, domain)?;

    let child = Command::new(program)
        .args(["tunnel", "--config"])
        .arg(&path)
        .args(["run", &request.name])
        .stdout(Stdio::null())
        // cloudflared logs to stderr. Sending it to the daemon's own
        // stderr puts it in the daemon log, which is where anyone
        // looking for "why is the tunnel down" will look.
        .stderr(Stdio::inherit())
        // See [`TunnelProcess`]: without this, dropping one leaves a
        // tunnel published with nothing managing it.
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| spawn_error(program, err))?;

    tracing::info!(
        "cloudflared tunnel `{}` running for *.{domain}",
        request.name
    );

    Ok((
        TunnelProcess {
            child,
            hostnames: Hostnames::Wildcard {
                domain: domain.to_string(),
            },
        },
        dns,
    ))
}

/// Creates the named tunnel unless it is already there.
pub async fn ensure_tunnel(program: &str, request: &TunnelRequest) -> Result<()> {
    run(
        program,
        &["tunnel", "create", &request.name],
        "creating the tunnel",
    )
    .await
    .map(|_| ())
}

/// Points the zone's wildcard hostname at the tunnel.
///
/// The outcome is returned rather than swallowed. `*.{zone}` is a record a
/// zone may well already have for its own reasons, and cloudflared refuses
/// with "already exists" without saying what holds the name — a record
/// that is not this tunnel means every Kobune hostname silently goes
/// nowhere, which is worth saying out loud.
pub async fn ensure_dns(
    program: &str,
    request: &TunnelRequest,
    domain: &str,
) -> Result<StepOutcome> {
    let record = format!("*.{domain}");

    run(
        program,
        &["tunnel", "route", "dns", &request.name, &record],
        format!("routing {record}"),
    )
    .await
}

/// Whether anything under the domain actually resolves.
///
/// **`route dns` exiting 0 does not mean the name exists.** The
/// certificate `cloudflared tunnel login` leaves behind is scoped to one
/// zone, and a hostname outside it is taken as a name *relative to* that
/// zone: `--domain 1024.works` against a login for `example.com` creates
/// `*.1024.works.example.com`, reports success, and leaves `*.1024.works`
/// never having existed. Every layer above then says `running` about a
/// tunnel no URL reaches.
///
/// Asking the resolver is the cheapest thing that can tell the difference,
/// and it needs no API token. A name nothing else would answer is used, so
/// what comes back is the wildcard or nothing.
///
/// `false` is "it did not answer", which a moment after the write can also
/// mean a cached NXDOMAIN — so the caller reports it as something to look
/// at rather than as a failure.
pub async fn wildcard_resolves(domain: &str) -> bool {
    let probe = format!("kobune-probe.{domain}:443");

    match tokio::net::lookup_host(&probe).await {
        Ok(mut addresses) => addresses.next().is_some(),
        Err(err) => {
            tracing::debug!("{probe} does not resolve: {err}");
            false
        }
    }
}

/// What a setup step actually did.
///
/// Every step runs every time, so "already exists" is success — but for
/// the DNS record it is also the one case Kobune cannot see past, so the
/// two are told apart rather than collapsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepOutcome {
    /// cloudflared did it, or confirmed it was already so.
    ///
    /// Not the same as "created it": `route dns` accepts a record that
    /// already points at this tunnel too, and exit 0 does not say which
    /// happened. It does not need to — either way the name arrives here.
    Done,
    /// cloudflared refused because the name is taken.
    ///
    /// By what, it does not say. For the DNS record that is the case
    /// Kobune cannot see past.
    AlreadyThere,
}

async fn run(program: &str, args: &[&str], operation: impl Into<String>) -> Result<StepOutcome> {
    let operation = operation.into();

    let output = tokio::time::timeout(SETUP_TIMEOUT, Command::new(program).args(args).output())
        .await
        .map_err(|_| {
            TunnelError::failed(
                operation.clone(),
                format!(
                    "cloudflared did not answer within {} seconds",
                    SETUP_TIMEOUT.as_secs()
                ),
            )
        })?
        .map_err(|err| spawn_error(program, err))?;

    if output.status.success() {
        return Ok(StepOutcome::Done);
    }

    let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let message = if message.is_empty() {
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    } else {
        message
    };

    if is_already_done(&message) {
        tracing::debug!("{operation}: already in place");
        return Ok(StepOutcome::AlreadyThere);
    }

    if message.contains("cert.pem") || message.contains("origincert") {
        return Err(TunnelError::NotLoggedIn);
    }

    Err(TunnelError::failed(
        operation,
        if message.is_empty() {
            format!("exit code {}", output.status.code().unwrap_or(-1))
        } else {
            message
        },
    ))
}

fn is_already_done(message: &str) -> bool {
    let lowered = message.to_ascii_lowercase();
    ALREADY_EXISTS.iter().any(|marker| lowered.contains(marker))
}

fn spawn_error(program: &str, err: std::io::Error) -> TunnelError {
    if err.kind() == std::io::ErrorKind::NotFound {
        TunnelError::NotInstalled(program.to_string())
    } else {
        TunnelError::failed(format!("running {program}"), err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::testing::stub;

    const ZONE: &str = "example.com";

    fn request(dir: &std::path::Path) -> TunnelRequest {
        TunnelRequest::new(dir, 80).with_domain(Some(ZONE.to_string()))
    }

    #[tokio::test]
    async fn creating_an_existing_tunnel_succeeds() {
        // Enable runs the setup steps every time, so a machine that is
        // already set up must not fail on the second run.
        let dir = tempfile::tempdir().expect("tempdir");
        let program = stub(
            dir.path(),
            r#"echo 'tunnel with name kobune already exists' >&2; exit 1"#,
        );

        ensure_tunnel(&program, &request(dir.path()))
            .await
            .expect("treated as success");
    }

    #[tokio::test]
    async fn an_existing_dns_record_succeeds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = stub(
            dir.path(),
            r#"echo 'Failed to add route: code: 1003, reason: An A, AAAA, or CNAME record with that host already exists' >&2; exit 1"#,
        );

        ensure_dns(&program, &request(dir.path()), ZONE)
            .await
            .expect("treated as success");
    }

    #[tokio::test]
    async fn a_missing_login_is_named_as_such() {
        // "cert.pem not found" tells the user nothing. The fix is to log
        // in, so that has to be what comes back.
        let dir = tempfile::tempdir().expect("tempdir");
        let program = stub(
            dir.path(),
            r#"echo 'Cannot determine default origin certificate path. No file cert.pem in ~/.cloudflared' >&2; exit 1"#,
        );

        let err = ensure_tunnel(&program, &request(dir.path()))
            .await
            .unwrap_err();
        assert!(matches!(err, TunnelError::NotLoggedIn), "got: {err}");
    }

    #[tokio::test]
    async fn other_failures_carry_cloudflared_s_own_message() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = stub(dir.path(), r#"echo 'zone not found' >&2; exit 1"#);

        let err = ensure_dns(&program, &request(dir.path()), ZONE)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("zone not found"), "got: {err}");
        assert!(
            err.to_string().contains("*.example.com"),
            "the operation says which record: {err}"
        );
    }

    #[tokio::test]
    async fn a_missing_cloudflared_is_named_as_such() {
        let err = ensure_tunnel(
            "/nonexistent/cloudflared",
            &request(std::path::Path::new("/tmp")),
        )
        .await
        .unwrap_err();

        assert!(matches!(err, TunnelError::NotInstalled(_)), "got: {err}");
    }

    #[tokio::test]
    async fn routes_the_zone_before_running() {
        // Without the DNS record nothing reaches the tunnel, and nothing
        // about the running tunnel would say why.
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("calls.log");
        let program = stub(
            dir.path(),
            &format!(
                r#"echo "$@" >> {}
if [ "$2" = "run" ] || [ "$4" = "run" ]; then sleep 30; fi"#,
                log.display()
            ),
        );

        let (tunnel, _) = start(&program, &request(dir.path()), ZONE)
            .await
            .expect("starts");
        Box::new(tunnel).stop().await;

        let calls = std::fs::read_to_string(&log).expect("reads");
        assert!(calls.contains("*.example.com"), "got:\n{calls}");
    }

    #[tokio::test]
    async fn a_dead_child_is_visible() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = stub(dir.path(), "exit 0");

        let (mut tunnel, _) = start(&program, &request(dir.path()), ZONE)
            .await
            .expect("starts");

        // The stub exits immediately; give it a moment to be reaped.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!tunnel.is_running(), "an exited cloudflared reads as down");
    }
}
