//! Listening on the Unix socket, and handling one connection's messages.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use minato_api::{
    ApiError, ClientMessage, MessageStream, Request, RequestId, ServerMessage, write_message,
};
use minato_runtime::EventSink;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, Notify, mpsc};

use crate::supervisor::{ClientStream, FromClient, Supervisor};

pub struct Server {
    socket: PathBuf,
    listener: UnixListener,
    shutdown: Arc<Notify>,
}

impl Server {
    /// Claims the socket, or reports that another daemon holds it.
    ///
    /// **Called before anything else starts.** launchd demand-launches this
    /// job whenever something reaches port 80, and if that happens while a
    /// second daemon is already resident, the loser has to stand down before
    /// it adopts launchd's listeners — otherwise it takes 80 and 443 away
    /// from the daemon that is actually serving.
    pub fn bind(socket: PathBuf, shutdown: Arc<Notify>) -> anyhow::Result<Option<Self>> {
        let Some(listener) = bind(&socket)? else {
            return Ok(None);
        };

        Ok(Some(Self {
            socket,
            listener,
            shutdown,
        }))
    }

    /// Accepts connections until told to stop.
    pub async fn run(self, supervisor: Arc<Supervisor>) -> anyhow::Result<()> {
        let listener = self.listener;
        tracing::info!("listening on {}", self.socket.display());

        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    match accepted {
                        Ok((stream, _)) => {
                            let supervisor = supervisor.clone();
                            tokio::spawn(async move {
                                if let Err(err) = handle_connection(stream, supervisor).await {
                                    tracing::debug!("finished handling the connection: {err}");
                                }
                            });
                        }
                        Err(err) => {
                            tracing::warn!("cannot accept the connection: {err}");
                        }
                    }
                }
                _ = self.shutdown.notified() => {
                    tracing::info!("asked to stop");
                    break;
                }
            }
        }

        // Clean up, so the next start can bind.
        let _ = std::fs::remove_file(&self.socket);
        Ok(())
    }
}

/// Creates the socket, clearing away anything the last run left behind.
///
/// `None` means a live daemon already owns it. **Not an error**: launchd
/// restarts this job on a non-zero exit (`KeepAlive { SuccessfulExit:
/// false }`), so bailing here would relaunch, lose the race again, and bail
/// again — a crash loop bounded only by launchd's throttle.
fn bind(socket: &Path) -> anyhow::Result<Option<UnixListener>> {
    if socket.exists() {
        if std::os::unix::net::UnixStream::connect(socket).is_ok() {
            return Ok(None);
        }
        std::fs::remove_file(socket)?;
    }

    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }

    Ok(Some(UnixListener::bind(socket)?))
}

/// Handles one connection.
///
/// Each request runs in its own task, and the read loop keeps going. That
/// is what makes cancellation possible at all: the loop used to await the
/// request inline, so a `Cancel` sat unread in the socket until the work it
/// referred to had already finished.
///
/// Requests on one connection can therefore overlap. The protocol was built
/// for it — every message carries the id it belongs to — and the CLI sends
/// one at a time regardless.
async fn handle_connection(stream: UnixStream, supervisor: Arc<Supervisor>) -> anyhow::Result<()> {
    let (read_half, write_half) = stream.into_split();
    let mut reader = MessageStream::new(read_half);

    // The event pumps and the final responses share one writer.
    let writer = Arc::new(Mutex::new(write_half));
    let mut running: HashMap<RequestId, tokio::task::JoinHandle<()>> = HashMap::new();

    // Where keystrokes and window sizes go, per request. Only an
    // interactive `logs` reads from its channel; for every other request
    // nothing is ever sent and the end of it is dropped below.
    let mut typing: HashMap<RequestId, mpsc::UnboundedSender<FromClient>> = HashMap::new();

    while let Some(message) = reader.recv::<ClientMessage>().await? {
        // Finished work is cleared out here rather than by the tasks
        // themselves, which would mean sharing the map across them.
        running.retain(|_, handle| !handle.is_finished());
        typing.retain(|id, _| running.contains_key(id));

        match message {
            ClientMessage::Request { id, request } => {
                let supervisor = supervisor.clone();
                let writer = writer.clone();

                let (keys, from_client) = mpsc::unbounded_channel();
                typing.insert(id, keys);

                running.insert(
                    id,
                    tokio::spawn(async move {
                        serve(id, request, supervisor, writer, from_client).await
                    }),
                );
            }
            ClientMessage::Input { id, data } => {
                // A key pressed after the program exited is nobody's
                // mistake, and neither is one for a request that was never
                // attached. Both end here, quietly.
                if let Some(keys) = typing.get(&id) {
                    let _ = keys.send(FromClient::Keys(minato_api::decode_bytes(&data)));
                }
            }
            ClientMessage::Resize { id, window } => {
                if let Some(keys) = typing.get(&id) {
                    let _ = keys.send(FromClient::Resize(window));
                }
            }
            ClientMessage::Cancel { id } => {
                typing.remove(&id);

                let Some(handle) = running.remove(&id) else {
                    // Already finished, or never started. Its response has
                    // gone out either way, so sending another would leave
                    // two for one id.
                    tracing::debug!("nothing in flight for request {id}");
                    continue;
                };

                handle.abort();

                // Aborting drops the task wherever it was, so a container
                // that was half-created stays half-created. `up` and `rm`
                // both recover from that, and the alternative — checking
                // for cancellation between every step — is a lot of
                // machinery for an operation someone has already given up
                // on.
                let mut guard = writer.lock().await;
                let response = ServerMessage::err(id, ApiError::cancelled());
                let _ = write_message(&mut *guard, &response).await;
            }
        }
    }

    Ok(())
}

/// Runs one request and writes its events and response.
async fn serve(
    id: RequestId,
    request: Request,
    supervisor: Arc<Supervisor>,
    writer: Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>,
    from_client: ClientStream,
) {
    let (sink, mut receiver) = EventSink::channel();

    // Write events out as they happen. They all have to land before the
    // response does, hence the join below.
    let event_writer = writer.clone();
    let pump = tokio::spawn(async move {
        while let Some(event) = receiver.recv().await {
            let mut guard = event_writer.lock().await;
            if write_message(&mut *guard, &ServerMessage::event(id, event))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    let outcome = supervisor.handle(request, &sink, from_client).await;

    // Dropping the sink closes the channel and ends the pump.
    drop(sink);
    let _ = pump.await;

    let response = match outcome {
        Ok(value) => ServerMessage::ok(id, value),
        Err(error) => ServerMessage::err(id, error),
    };

    let mut guard = writer.lock().await;
    let _ = write_message(&mut *guard, &response).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stands_down_rather_than_failing_over_a_live_daemon() {
        // An error here exits non-zero, and launchd restarts a job that
        // exits non-zero. Losing the race has to look like success.
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("minatod.sock");

        let _listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");

        assert!(bind(&socket).expect("not an error").is_none());
    }

    #[tokio::test]
    async fn replaces_a_stale_socket_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("minatod.sock");

        // Leftovers nobody is listening on.
        std::fs::write(&socket, b"").expect("creates it");

        let listener = bind(&socket).expect("clears the leftovers and binds");
        assert!(listener.is_some());
    }

    #[tokio::test]
    async fn creates_the_parent_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("nested").join("minatod.sock");

        let listener = bind(&socket).expect("creates the parent too");
        assert!(socket.exists());
        drop(listener);
    }
}
