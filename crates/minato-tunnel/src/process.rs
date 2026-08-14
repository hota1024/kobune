//! Driving the `cloudflared` CLI, and owning the process that runs.
//!
//! Setup goes through the CLI rather than Cloudflare's HTTP API so there
//! is no API token to obtain, store or scope. `cloudflared tunnel login`
//! already leaves a certificate behind, and `create` and `route dns` use
//! it. The one thing the CLI cannot do is apply an Access policy, which is
//! why exposing a tunnel without one has to be asked for explicitly.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, Command};

use crate::{Result, TunnelError, TunnelSettings, config};

/// How long a setup command may take.
///
/// These reach Cloudflare's API, so they are not instant, but any of them
/// still sitting here after this is not going to finish.
const SETUP_TIMEOUT: Duration = Duration::from_secs(30);

/// What `cloudflared` says when the work is already done.
///
/// Both setup steps are run on every enable, so "it exists" has to read as
/// success — otherwise re-running `minato tunnel enable` fails on a
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
/// managing any more.
#[derive(Debug)]
pub struct TunnelProcess {
    child: Child,
    settings: TunnelSettings,
}

impl TunnelProcess {
    /// Prepares the tunnel and starts it.
    ///
    /// Creating the tunnel and routing DNS are both idempotent, so this is
    /// safe to call on every daemon start rather than only the first.
    ///
    /// The DNS outcome comes back with the process because the caller is
    /// the only one that knows whether Minato has routed this zone before,
    /// and so whether "already existed" is its own record or a stranger's.
    pub async fn start(settings: TunnelSettings) -> Result<(Self, StepOutcome)> {
        ensure_tunnel(&settings).await?;
        let dns = ensure_dns(&settings).await?;

        let path = config::write_config(&settings)?;

        let child = Command::new(&settings.program)
            .args(["tunnel", "--config"])
            .arg(&path)
            .args(["run", &settings.name])
            .stdout(Stdio::null())
            // cloudflared logs to stderr. Sending it to the daemon's own
            // stderr puts it in the daemon log, which is where anyone
            // looking for "why is the tunnel down" will look.
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|err| spawn_error(&settings.program, err))?;

        tracing::info!(
            "cloudflared tunnel `{}` running for *.{}",
            settings.name,
            settings.domain
        );

        Ok((Self { child, settings }, dns))
    }

    pub fn settings(&self) -> &TunnelSettings {
        &self.settings
    }

    /// Whether the child is still alive.
    ///
    /// cloudflared exiting on its own — a revoked credential, a deleted
    /// tunnel — otherwise goes unnoticed, and every tunnel URL quietly
    /// stops working while `status` still claims it is up.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Stops the tunnel.
    pub async fn stop(mut self) {
        if let Err(err) = self.child.kill().await {
            tracing::debug!("cannot stop cloudflared: {err}");
        }
    }
}

/// Creates the named tunnel unless it is already there.
pub async fn ensure_tunnel(settings: &TunnelSettings) -> Result<()> {
    run(
        settings,
        &["tunnel", "create", &settings.name],
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
/// that is not this tunnel means every Minato hostname silently goes
/// nowhere, which is worth saying out loud.
pub async fn ensure_dns(settings: &TunnelSettings) -> Result<StepOutcome> {
    let record = settings.dns_record();

    run(
        settings,
        &["tunnel", "route", "dns", &settings.name, &record],
        format!("routing {record}"),
    )
    .await
}

/// Deletes the named tunnel.
///
/// `disable` does not call this: the tunnel is machine-wide and cheap to
/// leave in place, and deleting it would mean another `login`-scoped round
/// trip to bring it back. It exists for tearing a machine down.
pub async fn delete_tunnel(settings: &TunnelSettings) -> Result<()> {
    run(
        settings,
        &["tunnel", "delete", "--force", &settings.name],
        "deleting the tunnel",
    )
    .await
    .map(|_| ())
}

/// What a setup step actually did.
///
/// Every step runs every time, so "already exists" is success — but for
/// the DNS record it is also the one case Minato cannot see past, so the
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
    /// Minato cannot see past.
    AlreadyThere,
}

async fn run(
    settings: &TunnelSettings,
    args: &[&str],
    operation: impl Into<String>,
) -> Result<StepOutcome> {
    let operation = operation.into();

    let output = tokio::time::timeout(
        SETUP_TIMEOUT,
        Command::new(&settings.program).args(args).output(),
    )
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
    .map_err(|err| spawn_error(&settings.program, err))?;

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

/// The path a generated configuration would be written to.
pub fn config_path(settings: &TunnelSettings) -> PathBuf {
    settings.config_path()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A stand-in for `cloudflared` that reports what it was asked and
    /// exits how the test wants.
    ///
    /// The real thing needs a Cloudflare account, so the argument handling
    /// and the error mapping are what can be pinned down here.
    fn stub(dir: &std::path::Path, script: &str) -> String {
        let path = dir.join("cloudflared-stub");
        let mut file = std::fs::File::create(&path).expect("creates");
        // The probe below has to be answerable without running the body,
        // or waiting for the stub would be a call the test can see.
        write!(
            file,
            "#!/bin/sh\n[ \"$1\" = --probe ] && exit 0\n{script}\n"
        )
        .expect("writes");
        drop(file);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
            wait_until_runnable(&path);
        }

        path.to_string_lossy().to_string()
    }

    /// Waits for the kernel to allow the stub to be executed.
    ///
    /// Tests run in parallel, and a sibling spawning a process in the
    /// window where this file is open for writing leaves that child
    /// holding a writer to it until it execs. Linux refuses to run a file
    /// anything has open for writing, so the first spawn of a stub could
    /// fail with `ETXTBSY` for a reason that has nothing to do with what
    /// the test is checking. The window shuts on its own; wait for it here
    /// instead of letting a test fail on it.
    #[cfg(unix)]
    fn wait_until_runnable(path: &std::path::Path) {
        for _ in 0..500 {
            match std::process::Command::new(path).arg("--probe").status() {
                Ok(_) => return,
                Err(err) if err.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(err) => panic!("cannot run the stub at {}: {err}", path.display()),
            }
        }

        panic!("{} never stopped being busy", path.display());
    }

    fn settings(dir: &std::path::Path, script: &str) -> TunnelSettings {
        TunnelSettings::new("example.com", dir, 80).with_program(stub(dir, script))
    }

    #[tokio::test]
    async fn creating_an_existing_tunnel_succeeds() {
        // Enable runs the setup steps every time, so a machine that is
        // already set up must not fail on the second run.
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = settings(
            dir.path(),
            r#"echo 'tunnel with name minato already exists' >&2; exit 1"#,
        );

        ensure_tunnel(&settings).await.expect("treated as success");
    }

    #[tokio::test]
    async fn an_existing_dns_record_succeeds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = settings(
            dir.path(),
            r#"echo 'Failed to add route: code: 1003, reason: An A, AAAA, or CNAME record with that host already exists' >&2; exit 1"#,
        );

        ensure_dns(&settings).await.expect("treated as success");
    }

    #[tokio::test]
    async fn a_missing_login_is_named_as_such() {
        // "cert.pem not found" tells the user nothing. The fix is to log
        // in, so that has to be what comes back.
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = settings(
            dir.path(),
            r#"echo 'Cannot determine default origin certificate path. No file cert.pem in ~/.cloudflared' >&2; exit 1"#,
        );

        let err = ensure_tunnel(&settings).await.unwrap_err();
        assert!(matches!(err, TunnelError::NotLoggedIn), "got: {err}");
    }

    #[tokio::test]
    async fn other_failures_carry_cloudflared_s_own_message() {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = settings(dir.path(), r#"echo 'zone not found' >&2; exit 1"#);

        let err = ensure_dns(&settings).await.unwrap_err();
        assert!(err.to_string().contains("zone not found"), "got: {err}");
        assert!(
            err.to_string().contains("*.example.com"),
            "the operation says which record: {err}"
        );
    }

    #[tokio::test]
    async fn a_missing_cloudflared_is_named_as_such() {
        let settings =
            TunnelSettings::new("example.com", "/tmp", 80).with_program("/nonexistent/cloudflared");

        let err = ensure_tunnel(&settings).await.unwrap_err();
        assert!(matches!(err, TunnelError::NotInstalled(_)), "got: {err}");
    }

    #[tokio::test]
    async fn routes_the_zone_before_running() {
        // Without the DNS record nothing reaches the tunnel, and nothing
        // about the running tunnel would say why.
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("calls.log");
        let settings = settings(
            dir.path(),
            &format!(
                r#"echo "$@" >> {}
if [ "$2" = "run" ] || [ "$4" = "run" ]; then sleep 30; fi"#,
                log.display()
            ),
        );

        let (tunnel, _) = TunnelProcess::start(settings).await.expect("starts");
        tunnel.stop().await;

        let calls = std::fs::read_to_string(&log).expect("reads");
        assert!(calls.contains("*.example.com"), "got:\n{calls}");
    }

    #[tokio::test]
    async fn a_dead_child_is_visible() {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = settings(dir.path(), "exit 0");

        let (mut tunnel, _) = TunnelProcess::start(settings).await.expect("starts");

        // The stub exits immediately; give it a moment to be reaped.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!tunnel.is_running(), "an exited cloudflared reads as down");
    }
}
