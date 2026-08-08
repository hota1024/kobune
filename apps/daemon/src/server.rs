//! Listening on the Unix socket, and handling one connection's messages.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use minato_api::{ClientMessage, MessageStream, ServerMessage, write_message};
use minato_runtime::EventSink;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, Notify};

use crate::supervisor::Supervisor;

pub struct Server {
    socket: PathBuf,
    supervisor: Arc<Supervisor>,
    shutdown: Arc<Notify>,
}

impl Server {
    pub fn new(socket: PathBuf, supervisor: Arc<Supervisor>, shutdown: Arc<Notify>) -> Self {
        Self {
            socket,
            supervisor,
            shutdown,
        }
    }

    /// Starts listening, accepting connections until told to stop.
    pub async fn run(&self) -> anyhow::Result<()> {
        let listener = bind(&self.socket)?;
        tracing::info!("listening on {}", self.socket.display());

        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    match accepted {
                        Ok((stream, _)) => {
                            let supervisor = self.supervisor.clone();
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
fn bind(socket: &Path) -> anyhow::Result<UnixListener> {
    if socket.exists() {
        // A live daemon answers. That means a second instance, so stand
        // down.
        if std::os::unix::net::UnixStream::connect(socket).is_ok() {
            anyhow::bail!("a daemon is already running on {}", socket.display());
        }
        std::fs::remove_file(socket)?;
    }

    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }

    Ok(UnixListener::bind(socket)?)
}

async fn handle_connection(stream: UnixStream, supervisor: Arc<Supervisor>) -> anyhow::Result<()> {
    let (read_half, write_half) = stream.into_split();
    let mut reader = MessageStream::new(read_half);

    // The event pump and the final response share one writer.
    let writer = Arc::new(Mutex::new(write_half));

    while let Some(message) = reader.recv::<ClientMessage>().await? {
        match message {
            ClientMessage::Request { id, request } => {
                let (sink, mut receiver) = EventSink::channel();

                // Write events out as they happen. They all have to land
                // before the response does, hence the join below.
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

                let outcome = supervisor.handle(request, &sink).await;

                // Dropping the sink closes the channel and ends the pump.
                drop(sink);
                let _ = pump.await;

                let response = match outcome {
                    Ok(value) => ServerMessage::ok(id, value),
                    Err(error) => ServerMessage::err(id, error),
                };

                let mut guard = writer.lock().await;
                write_message(&mut *guard, &response).await?;
            }
            ClientMessage::Cancel { id } => {
                // M0 cannot cancel work in flight. Silently ignoring it
                // would leave the caller waiting, so at least log it.
                tracing::debug!("cancelling request {id} is not supported in M0");
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_to_bind_over_a_live_daemon() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("minatod.sock");

        let _listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");

        let err = bind(&socket).unwrap_err();
        assert!(err.to_string().contains("already running"), "got: {err}");
    }

    #[tokio::test]
    async fn replaces_a_stale_socket_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("minatod.sock");

        // Leftovers nobody is listening on.
        std::fs::write(&socket, b"").expect("creates it");

        let listener = bind(&socket).expect("clears the leftovers and binds");
        drop(listener);
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
