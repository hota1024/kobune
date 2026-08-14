//! Listening on the Unix socket, and handling one connection's messages.
//!
//! **The socket is the whole API, and it asks for nothing.** Whatever
//! reaches it can start containers, read logs and run commands inside
//! them — and `kobune exec <service> -- env` prints the secrets
//! `crate::secrets` resolved from 1Password and the Keychain. Keeping
//! those out of files buys nothing if anyone with an account on the
//! machine can ask for them down a socket, so who may connect is decided
//! here, in two places that back each other up: the mode on the directory
//! holding the socket, and the uid on the other end of each connection.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kobune_api::{
    ApiError, ClientMessage, MessageStream, Request, RequestId, ServerMessage, Typed, write_message,
};
use kobune_runtime::EventSink;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, Notify, mpsc};

use crate::supervisor::{ClientStream, Supervisor};

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
                            if !is_ours(&stream) {
                                // Dropping it is the refusal. Answering
                                // would say what is here to somebody who
                                // is not entitled to know.
                                tracing::warn!("refused a connection from another account");
                                continue;
                            }

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

    // **Before the bind, every time.** A directory nobody else may
    // traverse cannot be reached at all, which is the one guarantee that
    // also covers the instant between creating the socket and setting its
    // mode below. Applied to a directory that is already there as much as
    // to a new one: an installation that predates this would otherwise
    // stay open for as long as it lives.
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
        restrict(parent, HOME_MODE)?;
    }

    let listener = UnixListener::bind(socket)?;

    // Belt and braces, and the only thing left if `KOBUNE_HOME` names a
    // directory somebody else's account can reach.
    restrict(socket, SOCKET_MODE)?;

    Ok(Some(listener))
}

/// The mode kept on the directory holding the socket.
const HOME_MODE: u32 = 0o700;

/// The mode kept on the socket itself.
const SOCKET_MODE: u32 = 0o600;

/// Narrows `path` to `mode`, saying so when that was a change.
///
/// Reported rather than done quietly: a directory that has been readable
/// since it was created was readable to somebody, and finding that out
/// afterwards is worth a line in the log.
fn restrict(path: &Path, mode: u32) -> std::io::Result<()> {
    let current = std::fs::metadata(path)?.permissions().mode() & 0o777;
    if current == mode {
        return Ok(());
    }

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    tracing::info!(
        "narrowed {} from {current:04o} to {mode:04o}",
        path.display()
    );

    Ok(())
}

/// Whether the other end is the account the daemon runs as.
///
/// The directory mode already keeps everyone else out of the path, so
/// this is the second answer rather than the first. It is what covers a
/// `KOBUNE_HOME` pointed somewhere shared.
fn is_ours(stream: &UnixStream) -> bool {
    match peer_uid(stream) {
        Some(uid) => uid == unsafe { libc::geteuid() },
        // Nothing came back to compare. The directory is the guarantee
        // that matters, and refusing every connection because one
        // syscall did not answer would take the daemon out entirely.
        None => true,
    }
}

/// The uid on the other end of a connected socket.
///
/// Linux has no `getpeereid`; the same answer comes from `SO_PEERCRED`.
#[cfg(target_os = "linux")]
fn peer_uid(stream: &UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;

    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;

    // SAFETY: the descriptor is a connected socket this process owns, and
    // `length` describes the buffer being written into.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &mut length,
        )
    };

    (result == 0).then_some(credentials.uid)
}

#[cfg(not(target_os = "linux"))]
fn peer_uid(stream: &UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;

    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;

    // SAFETY: the descriptor is a connected socket this process owns;
    // both out parameters are writable.
    let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };

    (result == 0).then_some(uid)
}

/// A request that is still running, and the keyboard that reaches it.
struct InFlight {
    task: tokio::task::JoinHandle<()>,
    /// Where what the client types goes. Read only by an attached
    /// request; every other one lets it fill and drops it at the end.
    keys: mpsc::UnboundedSender<Typed>,
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
    let mut running: HashMap<RequestId, InFlight> = HashMap::new();

    while let Some(message) = reader.recv::<ClientMessage>().await? {
        // Finished work is cleared out here rather than by the tasks
        // themselves, which would mean sharing the map across them. The
        // keyboard goes with the task it belonged to.
        running.retain(|_, request| !request.task.is_finished());

        // Keystrokes and window sizes reach the request they name, if it
        // is still there and reading. One that is not attached never
        // reads its channel, and one that has finished is not in the map
        // at all: a key pressed a moment after the program exited is
        // nobody's mistake, and ends here quietly.
        if let Some(typed) = message.as_typed() {
            if let Some(request) = running.get(&message.request_id()) {
                let _ = request.keys.send(typed);
            }

            continue;
        }

        match message {
            ClientMessage::Request { id, request } => {
                let supervisor = supervisor.clone();
                let writer = writer.clone();

                let (keys, from_client) = mpsc::unbounded_channel();

                running.insert(
                    id,
                    InFlight {
                        task: tokio::spawn(async move {
                            serve(id, request, supervisor, writer, from_client).await
                        }),
                        keys,
                    },
                );
            }
            ClientMessage::Cancel { id } => {
                let Some(request) = running.remove(&id) else {
                    // Already finished, or never started. Its response has
                    // gone out either way, so sending another would leave
                    // two for one id.
                    tracing::debug!("nothing in flight for request {id}");
                    continue;
                };

                request.task.abort();

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
            // Answered above, by whichever request they name.
            ClientMessage::Input { .. } | ClientMessage::Resize { .. } => {}
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
        let socket = dir.path().join("kobuned.sock");

        let _listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");

        assert!(bind(&socket).expect("not an error").is_none());
    }

    #[tokio::test]
    async fn replaces_a_stale_socket_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("kobuned.sock");

        // Leftovers nobody is listening on.
        std::fs::write(&socket, b"").expect("creates it");

        let listener = bind(&socket).expect("clears the leftovers and binds");
        assert!(listener.is_some());
    }

    #[tokio::test]
    async fn creates_the_parent_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("nested").join("kobuned.sock");

        let listener = bind(&socket).expect("creates the parent too");
        assert!(socket.exists());
        drop(listener);
    }

    fn mode_of(path: &Path) -> u32 {
        std::fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777
    }

    #[tokio::test]
    async fn the_socket_and_its_directory_are_the_owner_s_alone() {
        // Nothing on the other side of this socket asks who is calling,
        // and `kobune exec -- env` prints resolved secrets. Under the
        // default umask the bind alone leaves it 0755.
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        let socket = home.join("kobuned.sock");

        let listener = bind(&socket).expect("binds").expect("is ours");

        assert_eq!(mode_of(&socket), SOCKET_MODE, "the socket");
        assert_eq!(mode_of(&home), HOME_MODE, "the directory holding it");

        drop(listener);
    }

    #[tokio::test]
    async fn a_directory_that_predates_this_is_narrowed_too() {
        // Every existing installation has a 0755 ~/.kobune. Tightening
        // only what this creates would leave all of them open.
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).expect("creates");
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let listener = bind(&home.join("kobuned.sock")).expect("binds");

        assert_eq!(mode_of(&home), HOME_MODE);
        drop(listener);
    }

    #[tokio::test]
    async fn a_connection_from_this_account_is_recognised() {
        // As far as one process can go: a second account to be refused
        // is not something a test can arrange. What is checked is that
        // the answer arrives at all — a `peer_uid` returning `None`
        // everywhere would let everything through and still pass a test
        // that only asserted acceptance.
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("kobuned.sock");

        let listener = bind(&socket).expect("binds").expect("is ours");
        let client = UnixStream::connect(&socket).await.expect("connects");
        let (accepted, _) = listener.accept().await.expect("accepts");

        assert_eq!(
            peer_uid(&accepted),
            Some(unsafe { libc::geteuid() }),
            "the uid has to actually come back, or the check is a no-op"
        );
        assert!(is_ours(&accepted));

        drop(client);
    }
}
