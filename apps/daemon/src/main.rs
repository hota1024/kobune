//! minatod — Minato's resident process.
//!
//! The port ledger, the reverse proxy, DNS and idle sweeping all need
//! something to stay running, hence a daemon. At M0 there was only the
//! supervisor; every milestone since has added to this.

mod activation;
mod activator;
mod env;
mod gateway;
mod idle;
mod resolve;
mod secrets;
mod server;
mod spec;
mod supervisor;
mod tunnel;

use std::sync::Arc;

use clap::Parser;
use minato_core::Paths;
use tokio::sync::Notify;
use tracing_subscriber::EnvFilter;

use crate::activator::{DeferredActivator, SupervisorActivator};
use crate::gateway::{Gateway, GatewaySettings};
use crate::server::Server;
use crate::supervisor::Supervisor;
use crate::tunnel::TunnelHandle;

/// `0.1.0 (abc1234)`. Every nightly reports the same version, so the commit
/// is what tells one build from another.
fn version() -> &'static str {
    // Leaked once at startup: clap wants a `&'static str`, and the string is
    // built from two compile-time constants so there is nothing to free.
    static VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    VERSION.get_or_init(|| minato_core::version_string(env!("CARGO_PKG_VERSION")))
}

#[derive(Parser, Debug)]
#[command(name = "minatod", version = version(), about = "Minato's resident process")]
struct Args {
    /// Also log to stderr. For debugging by hand.
    #[arg(long)]
    foreground: bool,

    /// How verbose to log. `MINATO_LOG` works too.
    #[arg(long, default_value = "info")]
    log_level: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let paths = Paths::resolve()?;
    paths.ensure()?;

    // Left until after the bind, this surfaces as an inscrutable
    // `SUN_LEN`. Catch it before logging is even up, and make sure it
    // reaches stderr.
    if let Err(err) = paths.check_socket_length() {
        eprintln!("error: {err}");
        return Err(err.into());
    }

    init_logging(&args, &paths)?;

    tracing::info!(
        "starting minatod {} (protocol {})",
        env!("CARGO_PKG_VERSION"),
        minato_api::PROTOCOL_VERSION
    );

    let shutdown = Arc::new(Notify::new());

    // The proxy and DNS come up first. A failed bind does not stop the
    // daemon; it just means no URLs are issued and only the direct
    // endpoints work.
    let settings = GatewaySettings::from_env();

    // The gateway needs somewhere to send wake requests, and the
    // supervisor needs the gateway to issue URLs. Supplying the real
    // implementation later breaks the cycle.
    let deferred = DeferredActivator::new();
    let gateway = Arc::new(
        Gateway::start(
            &paths,
            &settings,
            Arc::new(deferred.clone()),
            shutdown.clone(),
        )
        .await,
    );

    if !gateway.is_serving() {
        tracing::warn!(
            "the proxy is not listening, so no URLs will be issued. \
             Check `minato doctor`"
        );
    }

    let tunnel = TunnelHandle::new();
    let supervisor = Arc::new(Supervisor::new(
        &paths,
        gateway,
        tunnel.clone(),
        shutdown.clone(),
    ));
    deferred.set(Arc::new(SupervisorActivator::new(supervisor.clone())));

    // A tunnel that was on before the daemon restarted comes back up.
    // Anything else would leave the URLs handed to a reviewer dead until
    // somebody noticed and ran `tunnel enable` again.
    supervisor.restore_tunnel().await;

    // And the routing table with it. It lives in memory, so without this
    // every URL 404s until some command refreshes it — which a reviewer
    // holding a link has no way to trigger.
    supervisor.restore_routes().await;

    spawn_idle_sweeper(supervisor.clone(), shutdown.clone());
    let server = Server::new(paths.socket(), supervisor, shutdown.clone());

    // A signal and a Request::Shutdown stop it the same way.
    spawn_signal_handler(shutdown.clone());

    let result = server.run().await;

    // The tunnel does not outlive the daemon. Left running it would keep
    // publishing an environment nothing is managing any more.
    tunnel.stop().await;

    let _ = std::fs::remove_file(paths.pid_file());
    tracing::info!("minatod is shutting down");

    result
}

fn init_logging(args: &Args, paths: &Paths) -> anyhow::Result<()> {
    let filter =
        EnvFilter::try_from_env("MINATO_LOG").unwrap_or_else(|_| EnvFilter::new(&args.log_level));

    // A daemon started by the CLI has its stdout closed, so without a log
    // file there is nothing to go on at all.
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.daemon_log())?;

    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false);

    if args.foreground {
        subscriber.with_writer(std::io::stderr).init();
    } else {
        subscriber.with_writer(log_file).init();
    }

    Ok(())
}

/// Stops idle services on a timer.
///
/// This is what keeps the running containers down to the ones actually in
/// use, however many worktrees pile up.
fn spawn_idle_sweeper(supervisor: Arc<Supervisor>, shutdown: Arc<Notify>) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(idle::SWEEP_INTERVAL);
        // The first tick fires immediately and has nothing to sweep.
        ticker.tick().await;

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    let stopped = supervisor.sweep_idle().await;
                    if stopped > 0 {
                        tracing::info!("stopped {stopped} idle service(s)");
                    }
                }
                _ = shutdown.notified() => break,
            }
        }
    });
}

fn spawn_signal_handler(shutdown: Arc<Notify>) {
    tokio::spawn(async move {
        let mut terminate =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(signal) => signal,
                Err(err) => {
                    tracing::warn!("cannot listen for SIGTERM: {err}");
                    return;
                }
            };

        tokio::select! {
            _ = tokio::signal::ctrl_c() => tracing::info!("received SIGINT"),
            _ = terminate.recv() => tracing::info!("received SIGTERM"),
        }

        shutdown.notify_waiters();
    });
}
