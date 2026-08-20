//! The dashboard's end of the socket.
//!
//! Two channels and nothing else: commands go one way, updates come back
//! the other. The drawing side never awaits anything, which is what keeps
//! a minute-long `up` from freezing the screen it is being watched on.
//!
//! Modelled on the GUI's bridge (`apps/desktop/src/bridge.rs`), which
//! solves the same problem — and deliberately shares its numbers, because
//! the reason for them is the same one.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use kobune_api::{Diagnostics, EnvInfo, Event, LogLevel, Request, Response, Target, WorkspaceInfo};
use kobune_client::{Client, ClientError, Connection};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;

/// How often the listing is re-fetched.
///
/// Scale-to-zero stops services on its own, so a screen nobody touches
/// still goes out of date — the same reason the GUI polls at this
/// interval.
const REFRESH_INTERVAL: Duration = Duration::from_secs(3);

/// How long to wait after a failed connection before trying again.
const RETRY_INTERVAL: Duration = Duration::from_secs(5);

/// How much of the past a log pane opens on.
///
/// Enough to see what a service said as it started without waiting for it
/// to say anything more. The GUI's log pane asks for the same number.
const TAIL: usize = 200;

/// What the dashboard asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Re-fetch the listing now.
    Refresh,
    Up {
        path: PathBuf,
        services: Vec<String>,
    },
    Down {
        path: PathBuf,
        services: Vec<String>,
    },
    /// Follow a workspace's logs, or one service's.
    ///
    /// Replaces whatever was being followed. Only one stream runs at a
    /// time, because there is one pane to put it in.
    Follow {
        path: PathBuf,
        services: Vec<String>,
    },
    /// Stop following, and tell the daemon so.
    StopFollowing,
    /// `kobune doctor`, for the overlay.
    Checks { path: PathBuf },
    /// `kobune env ls`, for the overlay.
    Env {
        path: PathBuf,
        service: Option<String>,
    },
}

/// What comes back.
#[derive(Debug)]
pub enum Update {
    Listing(Vec<WorkspaceInfo>),
    /// One event from the operation in flight.
    Event(Event),
    /// That operation finished, one way or the other.
    Settled(Result<(), String>),
    /// Something went wrong that nobody asked for.
    Trouble(String),
    /// One line from the stream being followed.
    Log {
        /// Which service wrote it, where the daemon said. `kobune` for a
        /// line about the stream rather than from it.
        service: Option<String>,
        line: String,
    },
    /// The stream stopped on its own, and why when there was a reason.
    ///
    /// Not sent for a stream that was asked to stop: closing the pane is
    /// not news to the person who closed it.
    LogEnded(Option<String>),
    /// What `doctor` found.
    Checks(Box<Diagnostics>),
    /// What `env ls` listed.
    Env {
        entries: Vec<EnvInfo>,
        service: Option<String>,
    },
    /// One of those two could not be answered.
    InspectionFailed {
        what: &'static str,
        reason: String,
    },
}

/// Starts the socket side and hands back the two ends.
///
/// `polling` is the connection the first listing was fetched on. It is
/// kept for the ticker and nothing else: [`Connection`] handles one
/// request at a time, so an operation running on it would stop the screen
/// updating for as long as it took — which is exactly the minute somebody
/// is watching.
pub fn spawn(
    client: Client,
    cwd: PathBuf,
    polling: Connection,
) -> (UnboundedSender<Command>, UnboundedReceiver<Update>) {
    let (commands, from_ui) = tokio::sync::mpsc::unbounded_channel();
    let (to_ui, updates) = tokio::sync::mpsc::unbounded_channel();

    tokio::spawn(run(client, cwd, polling, from_ui, to_ui));

    (commands, updates)
}

async fn run(
    client: Client,
    cwd: PathBuf,
    polling: Connection,
    mut commands: UnboundedReceiver<Command>,
    updates: UnboundedSender<Update>,
) {
    let mut polling = Some(polling);
    let mut following: Option<Following> = None;
    let mut ticker = tokio::time::interval(REFRESH_INTERVAL);

    // The first tick fires at once, and the listing it would fetch has
    // just been fetched — that is what opened the screen.
    ticker.tick().await;

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                refresh(&client, &cwd, &mut polling, &updates).await;
            }

            command = commands.recv() => match command {
                Some(Command::Refresh) => {
                    refresh(&client, &cwd, &mut polling, &updates).await;
                }
                Some(Command::Follow { path, services }) => {
                    // One pane, one stream. The old one is told to stop
                    // rather than dropped, so the daemon lets go of the
                    // runtime's log stream now instead of when its next
                    // write fails.
                    if let Some(previous) = following.take() {
                        previous.stop();
                    }
                    following = Some(Following::start(
                        client.clone(),
                        path,
                        services,
                        updates.clone(),
                    ));
                }
                Some(Command::StopFollowing) => {
                    if let Some(previous) = following.take() {
                        previous.stop();
                    }
                }
                // A read, and a quick one, but on a connection of its
                // own all the same: the polling one is busy every three
                // seconds and this is somebody waiting on a keypress.
                Some(command @ (Command::Checks { .. } | Command::Env { .. })) => {
                    tokio::spawn(inspect(client.clone(), command, updates.clone()));
                }
                // Its own connection, and its own task: the ticker keeps
                // running underneath, so the states on screen catch up
                // while the operation is still going.
                Some(command) => {
                    tokio::spawn(operate(client.clone(), command, updates.clone()));
                }
                // The screen is gone.
                None => {
                    if let Some(previous) = following.take() {
                        previous.stop();
                    }
                    return;
                }
            },
        }
    }
}

/// Re-fetches the listing, reconnecting if the daemon went away.
async fn refresh(
    client: &Client,
    cwd: &Path,
    polling: &mut Option<Connection>,
    updates: &UnboundedSender<Update>,
) {
    let connection = match polling {
        Some(connection) => connection,
        None => match client.connect().await {
            Ok(connection) => polling.insert(connection),
            Err(err) => {
                let _ = updates.send(Update::Trouble(err.to_string()));
                tokio::time::sleep(RETRY_INTERVAL).await;
                return;
            }
        },
    };

    let request = Request::Ls {
        target: Target::new(cwd.to_path_buf()),
        all_projects: false,
    };

    match connection.request(request).await {
        Ok(Response::Workspaces { workspaces }) => {
            let _ = updates.send(Update::Listing(workspaces));
        }
        Ok(_) => {
            let _ = updates.send(Update::Trouble(
                "the daemon answered `ls` with something else".to_string(),
            ));
        }
        Err(err) => {
            // A refused or broken connection is worth a fresh one; an
            // error the daemon itself returned is not, and throwing the
            // connection away for one would reconnect every three seconds
            // in a directory with no `kobune.toml` in it.
            if !matches!(err, ClientError::Api(_)) {
                *polling = None;
            }

            let _ = updates.send(Update::Trouble(err.to_string()));
        }
    }
}

/// The log stream that is running, and the way to stop it.
struct Following {
    stop: oneshot::Sender<()>,
}

impl Following {
    /// Opens a stream and starts sending its lines back.
    fn start(
        client: Client,
        path: PathBuf,
        services: Vec<String>,
        updates: UnboundedSender<Update>,
    ) -> Self {
        let (stop, stopped) = oneshot::channel();

        tokio::spawn(follow(client, path, services, updates, stopped));

        Self { stop }
    }

    /// Asks it to stop.
    ///
    /// **Told rather than dropped.** Dropping the connection leaves the
    /// daemon holding the runtime's log stream until its next write
    /// fails, and `l` is a key somebody presses repeatedly. Cancelling
    /// reaches the request itself, which lets the stream go at once.
    fn stop(self) {
        let _ = self.stop.send(());
    }
}

/// Follows a workspace's logs until it is asked not to.
async fn follow(
    client: Client,
    path: PathBuf,
    services: Vec<String>,
    updates: UnboundedSender<Update>,
    stopped: oneshot::Receiver<()>,
) {
    let mut connection = match client.connect().await {
        Ok(connection) => connection,
        Err(err) => {
            let _ = updates.send(Update::LogEnded(Some(err.to_string())));
            return;
        }
    };

    let request = Request::Logs {
        target: Target::new(path),
        services,
        follow: true,
        tail: Some(TAIL),
        // A pane has no terminal to lend and nothing to type with, so
        // the offer is not made. The daemon then streams the logs for
        // every service rather than handing one of them the screen.
        attach: None,
    };

    // Which of the two ended it. Cancelling comes back as an error like
    // any other, and reporting "the log stream ended: cancelled" to
    // somebody who has just closed the pane would be answering their own
    // keystroke.
    let asked_to_stop = Arc::new(AtomicBool::new(false));

    let outcome = connection
        .call_until(
            request,
            |event| match event {
                Event::Output { service, line, .. } => {
                    let _ = updates.send(Update::Log { service, line });
                }
                // The daemon's own remark about the stream — a service
                // whose logs it could not read. Under its own name, so it
                // does not read as something a container printed.
                Event::Log {
                    level: LogLevel::Warn | LogLevel::Error,
                    message,
                } => {
                    let _ = updates.send(Update::Log {
                        service: Some("kobune".to_string()),
                        line: message,
                    });
                }
                _ => {}
            },
            {
                let asked_to_stop = Arc::clone(&asked_to_stop);
                async move {
                    let _ = stopped.await;
                    asked_to_stop.store(true, Ordering::SeqCst);
                }
            },
        )
        .await;

    if !asked_to_stop.load(Ordering::SeqCst) {
        let _ = updates.send(Update::LogEnded(outcome.err().map(|err| err.to_string())));
    }
}

/// Answers one of the overlays.
async fn inspect(client: Client, command: Command, updates: UnboundedSender<Update>) {
    let (request, what) = match command {
        Command::Checks { path } => (
            Request::Doctor {
                target: Target::new(path),
            },
            "the checks",
        ),
        Command::Env { path, service } => (
            Request::EnvList {
                target: Target::new(path),
                // **Masked.** These are the values §8's secret
                // references resolved out of 1Password and the Keychain,
                // and a dashboard is a screen somebody else can be
                // standing behind. `kobune env ls --reveal` is a
                // deliberate act and stays one.
                reveal: false,
                service,
            },
            "the environment",
        ),
        _ => return,
    };

    let failed = |reason: String| Update::InspectionFailed { what, reason };

    let mut connection = match client.connect().await {
        Ok(connection) => connection,
        Err(err) => {
            let _ = updates.send(failed(err.to_string()));
            return;
        }
    };

    let update = match connection.request(request).await {
        Ok(Response::Diagnostics(report)) => Update::Checks(Box::new(report)),
        Ok(Response::Env { entries, service }) => Update::Env { entries, service },
        Ok(_) => failed("the daemon answered with something else".to_string()),
        Err(err) => failed(err.to_string()),
    };

    let _ = updates.send(update);
}

/// Runs one operation, reporting its steps as they arrive.
async fn operate(client: Client, command: Command, updates: UnboundedSender<Update>) {
    let request = match command {
        Command::Up { path, services } => Request::Up {
            target: Target::new(path),
            services,
            rebuild: false,
        },
        Command::Down { path, services } => Request::Down {
            target: Target::new(path),
            services,
            all: false,
        },
        // Handled before this is reached. `Refresh` needs the polling
        // connection rather than one of its own, the two about logs are
        // a stream that outlives a single call, and the two overlays
        // are reads that report somewhere else.
        Command::Refresh
        | Command::Follow { .. }
        | Command::StopFollowing
        | Command::Checks { .. }
        | Command::Env { .. } => return,
    };

    let mut connection = match client.connect().await {
        Ok(connection) => connection,
        Err(err) => {
            let _ = updates.send(Update::Settled(Err(err.to_string())));
            return;
        }
    };

    let outcome = connection
        .call(request, |event| {
            let _ = updates.send(Update::Event(event));
        })
        .await;

    let _ = updates.send(Update::Settled(
        outcome.map(|_| ()).map_err(|err| err.to_string()),
    ));
}
