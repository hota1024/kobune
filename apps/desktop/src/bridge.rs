//! The bridge between the tokio thread and the UI.
//!
//! `minato-client` only runs on tokio, and GPUI has an executor of its
//! own. Rather than mix them, tokio gets its own thread and hands over
//! nothing but results.
//!
//! **Nothing here depends on the UI framework.** Notification is a plain
//! channel, [`Notifier`]; wiring that to rendering is the UI's job.

use std::path::{Path, PathBuf};
use std::time::Duration;

use minato_api::{Event, OutputStream, Request, Response, Target};
use minato_client::Client;

use crate::state::{Connection, LogLine, SharedState};

/// Tells the UI that the state changed.
///
/// Not *what* changed — the UI re-reads [`SharedState`]. Carrying diffs
/// would couple the two together.
#[derive(Clone)]
pub struct Notifier(tokio::sync::mpsc::UnboundedSender<()>);

impl Notifier {
    pub fn channel() -> (Self, tokio::sync::mpsc::UnboundedReceiver<()>) {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        (Self(sender), receiver)
    }

    /// Notifies. No receiver is not a failure.
    pub fn notify(&self) {
        let _ = self.0.send(());
    }
}

/// How often the listing is re-fetched.
///
/// Scale-to-zero stops services on its own, and the screen has to show
/// that, so this polls even when nobody touches anything.
const REFRESH_INTERVAL: Duration = Duration::from_secs(3);

/// How long to wait after a failed connection before trying again.
const RETRY_INTERVAL: Duration = Duration::from_secs(5);

/// What the renderer asks tokio to do.
#[derive(Debug, Clone)]
pub enum Command {
    /// Re-fetch the listing now.
    Refresh,
    /// Follow this workspace's logs.
    FollowLogs { workspace: String },
    /// Stop following logs.
    StopLogs,
    /// Start the services.
    Up { workspace: String },
    /// Stop the services.
    Down { workspace: String },
}

/// Starts the tokio side and returns the handle to send it work.
pub fn spawn(
    state: SharedState,
    cwd: PathBuf,
    notifier: Notifier,
) -> tokio::sync::mpsc::UnboundedSender<Command> {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();

    std::thread::Builder::new()
        .name("minato-bridge".to_string())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(err) => {
                    state.write(|state| {
                        state.connection = Some(Connection::Failed(format!(
                            "cannot create the runtime: {err}"
                        )));
                    });
                    notifier.notify();
                    return;
                }
            };

            runtime.block_on(run(state, cwd, notifier, receiver));
        })
        .expect("spawns the thread");

    sender
}

async fn run(
    state: SharedState,
    cwd: PathBuf,
    notifier: Notifier,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<Command>,
) {
    let mut ticker = tokio::time::interval(REFRESH_INTERVAL);
    let mut log_task: Option<tokio::task::JoinHandle<()>> = None;

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                refresh(&state, &cwd, &notifier).await;
            }
            command = commands.recv() => {
                match command {
                    Some(Command::Refresh) => refresh(&state, &cwd, &notifier).await,
                    Some(Command::FollowLogs { workspace }) => {
                        // Stop the previous subscription first, or
                        // several workspaces' logs end up interleaved.
                        if let Some(task) = log_task.take() {
                            task.abort();
                        }

                        state.write(|state| {
                            state.clear_logs();
                            state.log_target = Some(workspace.clone());
                        });

                        log_task = Some(tokio::spawn(follow_logs(
                            state.clone(),
                            cwd.clone(),
                            notifier.clone(),
                            workspace,
                        )));
                    }
                    Some(Command::Up { workspace }) => {
                        operate(&state, &cwd, &notifier, workspace, true).await;
                    }
                    Some(Command::Down { workspace }) => {
                        operate(&state, &cwd, &notifier, workspace, false).await;
                    }
                    Some(Command::StopLogs) => {
                        if let Some(task) = log_task.take() {
                            task.abort();
                        }
                        state.write(|state| state.log_target = None);
                        notifier.notify();
                    }
                    // The renderer is gone.
                    None => {
                        if let Some(task) = log_task.take() {
                            task.abort();
                        }
                        return;
                    }
                }
            }
        }
    }
}

/// Re-fetches the workspace listing.
async fn refresh(state: &SharedState, cwd: &Path, notifier: &Notifier) {
    let client = match Client::from_env() {
        Ok(client) => client,
        Err(err) => {
            set_failed(
                state,
                notifier,
                format!("cannot resolve the configuration: {err}"),
            );
            return;
        }
    };

    // The GUI never starts the daemon. Looking after it is launchd's job,
    // and a GUI managing it too would split that responsibility
    // (`docs/DESIGN.md` §15).
    let mut connection = match client.connect().await {
        Ok(connection) => connection,
        Err(err) => {
            set_failed(state, notifier, err.to_string());
            tokio::time::sleep(RETRY_INTERVAL).await;
            return;
        }
    };

    match connection.handshake().await {
        Ok(pong) => state.write(|state| {
            state.connection = Some(Connection::Connected(Box::new(pong)));
        }),
        Err(err) => {
            set_failed(state, notifier, err.to_string());
            return;
        }
    }

    let request = Request::Ls {
        target: Target::new(cwd.to_path_buf()),
        all_projects: false,
    };

    match connection.request(request).await {
        Ok(Response::Workspaces { workspaces }) => {
            tracing::debug!("fetched {} workspaces", workspaces.len());
            state.write(|state| {
                state.workspaces = workspaces;
                state.error = None;
            });
        }
        Ok(_) => state.write(|state| {
            state.error = Some("unexpected response from the daemon".to_string());
        }),
        Err(err) => state.write(|state| {
            // The connection is fine, so this is the listing's own
            // failure — started in a directory with no minato.toml, say.
            state.error = Some(err.to_string());
            state.workspaces.clear();
        }),
    }

    notifier.notify();
}

/// Starts or stops a workspace's services.
///
/// Waits for it to finish, then re-fetches. A button that changes nothing
/// when pressed looks broken, so being in progress is kept as state.
async fn operate(
    state: &SharedState,
    cwd: &Path,
    notifier: &Notifier,
    workspace: String,
    start: bool,
) {
    state.write(|state| {
        state.busy.insert(workspace.clone());
    });
    notifier.notify();

    let outcome = run_operation(cwd, &workspace, start).await;

    state.write(|state| {
        state.busy.remove(&workspace);

        if let Err(err) = &outcome {
            state.error = Some(err.clone());
        }
    });

    refresh(state, cwd, notifier).await;
}

async fn run_operation(cwd: &Path, workspace: &str, start: bool) -> Result<(), String> {
    let client = Client::from_env().map_err(|err| err.to_string())?;
    let mut connection = client.connect().await.map_err(|err| err.to_string())?;

    let target = Target::new(cwd.to_path_buf()).workspace(Some(workspace.to_string()));
    let request = if start {
        Request::Up {
            target,
            services: Vec::new(),
            // The button starts things; forcing a rebuild is a decision
            // with a cost, and there is nowhere in the UI that says so.
            rebuild: false,
        }
    } else {
        Request::Down {
            target,
            services: Vec::new(),
            all: false,
        }
    };

    connection
        .request(request)
        .await
        .map(|_| ())
        .map_err(|err| err.to_string())
}

/// Follows the logs, pushing each line that arrives into the state.
async fn follow_logs(state: SharedState, cwd: PathBuf, notifier: Notifier, workspace: String) {
    let Ok(client) = Client::from_env() else {
        return;
    };

    let Ok(mut connection) = client.connect().await else {
        state.write(|state| {
            state.push_log(LogLine {
                service: "minato".into(),
                line: "cannot connect to the daemon".into(),
                is_error: true,
            });
        });
        notifier.notify();
        return;
    };

    let request = Request::Logs {
        target: Target::new(cwd).workspace(Some(workspace)),
        services: Vec::new(),
        follow: true,
        tail: Some(200),
    };

    let outcome = connection
        .call(request, |event| {
            if let Event::Output {
                service,
                stream,
                line,
            } = event
            {
                state.write(|state| {
                    state.push_log(LogLine {
                        service: service.unwrap_or_else(|| "-".to_string()),
                        line,
                        is_error: stream == OutputStream::Stderr,
                    });
                });

                notifier.notify();
            }
        })
        .await;

    if let Err(err) = outcome {
        state.write(|state| {
            state.push_log(LogLine {
                service: "minato".into(),
                line: format!("log stream ended: {err}"),
                is_error: true,
            });
        });
        notifier.notify();
    }
}

fn set_failed(state: &SharedState, notifier: &Notifier, reason: String) {
    // Shown on screen only, a connection failure would leave no trace in
    // the logs.
    tracing::warn!("cannot connect to the daemon: {reason}");

    state.write(|state| {
        state.connection = Some(Connection::Failed(reason));
        state.workspaces.clear();
    });
    notifier.notify();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_interval_follows_scale_to_zero() {
        // An idle stop that never reaches the screen leaves a stopped
        // service looking like a running one.
        assert!(REFRESH_INTERVAL <= Duration::from_secs(5));
    }

    #[test]
    fn retry_is_slower_than_refresh() {
        // Do not hammer something that is not answering.
        assert!(RETRY_INTERVAL > REFRESH_INTERVAL);
    }
}
