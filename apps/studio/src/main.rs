//! `kobune-studio` — the dashboard, in a browser.
//!
//! A fourth client of the daemon, equal to the CLI, the GUI and Skills
//! (`docs/DESIGN.md` §3). It depends on `kobune-client` and `kobune-api`
//! and on no implementation: nothing here knows what Docker is, and the
//! screen is drawn from the same `ls` / `logs` / `doctor` the printed
//! commands use.
//!
//! **This binary puts an HTTP surface in front of a socket that asks for
//! nothing.** §3's access control is the mode on `KOBUNE_HOME` and the uid
//! on the far end of the socket, and a TCP listener has neither — so
//! everything that stands in for them is in [`guard`], and the listener
//! never leaves loopback. `kobune exec <service> -- env` prints resolved
//! secrets, so this is the part of the program that has to be right.

mod api;
mod assets;
mod guard;
mod model;
mod server;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use clap::Parser;

/// Where the dashboard listens when nothing says otherwise.
///
/// Fixed rather than left to the OS so that a reload after a restart lands
/// on the same page, and high enough to be out of the way of the dev
/// servers Kobune exists to run.
const DEFAULT_PORT: u16 = 17823;

#[derive(Parser, Debug)]
#[command(
    name = "kobune-studio",
    about = "The Kobune dashboard, served to a browser",
    version
)]
struct Args {
    /// The port to listen on.
    #[arg(long, env = "KOBUNE_STUDIO_PORT", default_value_t = DEFAULT_PORT)]
    port: u16,

    /// The project to show. Defaults to the working directory.
    #[arg(long)]
    path: Option<PathBuf>,

    /// The workspace to open first. The busiest one when left out.
    #[arg(long, short = 'w')]
    workspace: Option<String>,

    /// Print the URL instead of opening a browser.
    #[arg(long)]
    no_open: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "kobune_studio=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();

    let cwd = match args.path {
        Some(path) => std::fs::canonicalize(&path)
            .map_err(|err| anyhow::anyhow!("cannot read {}: {err}", path.display()))?,
        None => std::env::current_dir()?,
    };

    // Loopback, and not a flag. A `--host` would be the one setting that
    // turns "a page on this machine" into "anybody on this network may
    // start containers and read secrets", and §3 has no story for that
    // reader. Somebody who wants it can put their own proxy in front and
    // own the decision.
    let requested = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), args.port);

    let listener = tokio::net::TcpListener::bind(requested)
        .await
        .map_err(|err| {
            anyhow::anyhow!(
                "cannot listen on {requested}: {err}. Another studio may already have it"
            )
        })?;

    // **The bound address, not the requested one.** `--port 0` asks the
    // kernel to choose, and the session is what decides which `Host`
    // header is this server's — built from the request, it would compare
    // every arriving `Host` against port 0 and refuse all of them, having
    // already printed a URL nobody could open.
    let addr = listener
        .local_addr()
        .map_err(|err| anyhow::anyhow!("cannot read the port that was bound: {err}"))?;

    let session = guard::Session::new(addr)?;
    let url = session.entry_url(args.workspace.as_deref());

    eprintln!("kobune studio  {url}");
    if args.no_open {
        eprintln!("  open that URL. The token in the fragment is this session's.");
    } else if let Err(err) = open_browser(&url) {
        eprintln!("  cannot open a browser ({err}); open the URL above.");
    }

    server::serve(listener, session, cwd).await
}

/// Hands the URL to whatever the desktop opens links with.
///
/// The token rides in the fragment, which a browser does not send to the
/// server and does not put in `Referer`, so handing it over this way keeps
/// it out of the request log of the very server it authenticates to.
///
/// **It is an argument to another process, and that is visible.** On Linux
/// `/proc/<pid>/cmdline` is world-readable, so for as long as the opener
/// runs, a local process can read the token — the same local process the
/// token exists to keep out. The window is the lifetime of one `open`, and
/// there is no way to hand a URL to a browser that avoids it; `--no-open`
/// prints the URL instead and does not spawn anything.
fn open_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(not(target_os = "macos"))]
    let mut command = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };

    command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|_| ())
}
