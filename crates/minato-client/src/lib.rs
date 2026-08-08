//! Connecting to the daemon. Shared by the CLI and the GUI.
//!
//! This crate does not depend on `minato-runtime`. Letting a client touch
//! the runtime directly would undo the principle that the daemon's API is
//! the product (`docs/DESIGN.md` §3, §13).

use std::path::{Path, PathBuf};
use std::time::Duration;

use minato_api::{
    ApiError, ClientMessage, Event, MessageStream, PROTOCOL_VERSION, Pong, Request, RequestId,
    Response, ServerMessage, write_message,
};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

/// How long to wait for the daemon to come up.
const SPAWN_TIMEOUT: Duration = Duration::from_secs(10);

/// How often to retry while waiting.
const SPAWN_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The daemon's executable name.
const DAEMON_PROGRAM: &str = "minatod";

/// Points at the daemon explicitly.
pub const DAEMON_ENV: &str = "MINATO_DAEMON";

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("cannot connect to the daemon ({path}): {source}")]
    Connect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("cannot start the daemon: {0}")]
    Spawn(String),

    #[error("waited {}s for the daemon to start, with no response", SPAWN_TIMEOUT.as_secs())]
    SpawnTimeout,

    #[error("the connection to the daemon was closed")]
    Disconnected,

    #[error(transparent)]
    Codec(#[from] minato_api::CodecError),

    /// The daemon refused. Callers can display this as-is.
    #[error("{0}")]
    Api(#[source] ApiError),

    #[error("the daemon returned an unexpected response: {0}")]
    Protocol(String),

    #[error(
        "the daemon speaks protocol {server}, which this minato (protocol \
         {client}) cannot talk to. Restart it with `minato daemon restart`"
    )]
    VersionMismatch { client: u32, server: u32 },
}

impl ClientError {
    /// The value the CLI returns as its exit code.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Api(err) => err.code.exit_code(),
            _ => 1,
        }
    }

    /// The remedy to show the user.
    pub fn hint(&self) -> Option<&str> {
        match self {
            Self::Api(err) => err.hint.as_deref(),
            Self::Connect { .. } => Some("start it with `minato daemon start`"),
            _ => None,
        }
    }
}

/// Creates connections to the daemon.
#[derive(Debug, Clone)]
pub struct Client {
    socket: PathBuf,
    daemon_program: Option<PathBuf>,
}

impl Client {
    pub fn new(socket: PathBuf) -> Self {
        Self {
            socket,
            daemon_program: None,
        }
    }

    /// Builds one from the default paths (`$MINATO_HOME` or `~/.minato`).
    ///
    /// Also checks the socket path length: when it is too long the daemon
    /// dies right after starting, and from here that looks like silence.
    pub fn from_env() -> minato_core::Result<Self> {
        let paths = minato_core::Paths::resolve()?;
        paths.check_socket_length()?;
        Ok(Self::new(paths.socket()))
    }

    /// Points at the daemon explicitly. Otherwise found via the
    /// environment and PATH.
    pub fn with_daemon_program(mut self, program: PathBuf) -> Self {
        self.daemon_program = Some(program);
        self
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    /// Connects to an already-running daemon.
    pub async fn connect(&self) -> Result<Connection, ClientError> {
        let stream =
            UnixStream::connect(&self.socket)
                .await
                .map_err(|source| ClientError::Connect {
                    path: self.socket.clone(),
                    source,
                })?;

        Ok(Connection::new(stream))
    }

    /// Starts the daemon first if nothing answers.
    ///
    /// The CLI uses this by default, so the daemon stays invisible.
    pub async fn connect_or_spawn(&self) -> Result<Connection, ClientError> {
        match self.connect().await {
            Ok(connection) => return Ok(connection),
            Err(err) => {
                tracing::debug!("cannot connect, trying to start the daemon: {err}");
            }
        }

        // A crashed daemon leaves its socket file behind, and that stale
        // file makes bind fail. Remove anything that does not answer.
        if self.socket.exists() {
            let _ = std::fs::remove_file(&self.socket);
        }

        self.spawn_daemon()?;

        let deadline = std::time::Instant::now() + SPAWN_TIMEOUT;
        loop {
            tokio::time::sleep(SPAWN_POLL_INTERVAL).await;

            if let Ok(connection) = self.connect().await {
                return Ok(connection);
            }

            if std::time::Instant::now() >= deadline {
                return Err(ClientError::SpawnTimeout);
            }
        }
    }

    fn spawn_daemon(&self) -> Result<(), ClientError> {
        let program = self.resolve_daemon_program()?;

        // The daemon must outlive its parent. Detach the standard streams;
        // the daemon writes its own log to a file.
        std::process::Command::new(&program)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|err| {
                ClientError::Spawn(format!("cannot start {}: {err}", program.display()))
            })?;

        Ok(())
    }

    fn resolve_daemon_program(&self) -> Result<PathBuf, ClientError> {
        if let Some(program) = &self.daemon_program {
            return Ok(program.clone());
        }

        if let Some(value) = std::env::var_os(DAEMON_ENV) {
            return Ok(PathBuf::from(value));
        }

        // The CLI and the daemon ship together, so the daemon is probably
        // next door. Looking here before PATH makes a development build in
        // target/debug win.
        if let Ok(current) = std::env::current_exe() {
            if let Some(dir) = current.parent() {
                let sibling = dir.join(DAEMON_PROGRAM);
                if sibling.is_file() {
                    return Ok(sibling);
                }
            }
        }

        Ok(PathBuf::from(DAEMON_PROGRAM))
    }
}

/// An established connection.
///
/// Requests are handled one at a time. The protocol already supports
/// multiplexing, so concurrency can be added here when it is needed.
pub struct Connection {
    reader: MessageStream<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    next_id: u64,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("next_id", &self.next_id)
            .finish_non_exhaustive()
    }
}

impl Connection {
    fn new(stream: UnixStream) -> Self {
        let (read_half, write_half) = stream.into_split();

        Self {
            reader: MessageStream::new(read_half),
            writer: write_half,
            next_id: 1,
        }
    }

    fn take_id(&mut self) -> RequestId {
        let id = RequestId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Sends a request, passing each event to `on_event` while awaiting
    /// the response.
    pub async fn call<F>(
        &mut self,
        request: Request,
        mut on_event: F,
    ) -> Result<Response, ClientError>
    where
        F: FnMut(Event),
    {
        let id = self.take_id();

        write_message(&mut self.writer, &ClientMessage::Request { id, request }).await?;

        loop {
            let message: ServerMessage =
                self.reader.recv().await?.ok_or(ClientError::Disconnected)?;

            match message {
                ServerMessage::Event {
                    id: event_id,
                    event,
                } => {
                    // Events for other requests are not the caller's business.
                    if event_id == id {
                        on_event(event);
                    }
                }
                ServerMessage::Response {
                    id: response_id,
                    outcome,
                } if response_id == id => {
                    return outcome.into_result().map_err(ClientError::Api);
                }
                ServerMessage::Response { .. } => {}
                ServerMessage::Fatal { message } => {
                    return Err(ClientError::Protocol(message));
                }
            }
        }
    }

    /// Discards events and returns only the response.
    pub async fn request(&mut self, request: Request) -> Result<Response, ClientError> {
        self.call(request, |_| {}).await
    }

    /// Connectivity check and protocol handshake.
    pub async fn handshake(&mut self) -> Result<Pong, ClientError> {
        let response = self.request(Request::Ping).await?;

        let Response::Pong(pong) = response else {
            return Err(ClientError::Protocol(
                "Ping was answered with something other than Pong".to_string(),
            ));
        };

        if pong.protocol != PROTOCOL_VERSION {
            return Err(ClientError::VersionMismatch {
                client: PROTOCOL_VERSION,
                server: pong.protocol,
            });
        }

        Ok(pong)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minato_api::{ErrorCode, Outcome};
    use tokio::net::UnixListener;

    /// A stand-in daemon that replays canned responses.
    async fn serve(listener: UnixListener, responses: Vec<Vec<ServerMessage>>) {
        let (stream, _) = listener.accept().await.expect("accepts");
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = MessageStream::new(read_half);

        for batch in responses {
            let Some(_request): Option<ClientMessage> = reader.recv().await.expect("reads") else {
                return;
            };

            for message in batch {
                write_message(&mut write_half, &message)
                    .await
                    .expect("writes");
            }
        }
    }

    fn socket_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("minatod.sock")
    }

    fn pong(protocol: u32) -> Response {
        Response::Pong(Pong {
            version: "0.1.0".into(),
            protocol,
            runtime: "docker".into(),
            uptime_secs: 1,
        })
    }

    #[tokio::test]
    async fn performs_handshake() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = socket_path(&dir);
        let listener = UnixListener::bind(&path).expect("bind");

        tokio::spawn(serve(
            listener,
            vec![vec![ServerMessage::ok(
                RequestId(1),
                pong(PROTOCOL_VERSION),
            )]],
        ));

        let mut connection = Client::new(path).connect().await.expect("connects");
        let result = connection.handshake().await.expect("succeeds");
        assert_eq!(result.runtime, "docker");
    }

    #[tokio::test]
    async fn rejects_incompatible_protocol() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = socket_path(&dir);
        let listener = UnixListener::bind(&path).expect("bind");

        tokio::spawn(serve(
            listener,
            vec![vec![ServerMessage::ok(
                RequestId(1),
                pong(PROTOCOL_VERSION + 1),
            )]],
        ));

        let mut connection = Client::new(path).connect().await.expect("connects");
        let err = connection.handshake().await.unwrap_err();

        assert!(
            matches!(err, ClientError::VersionMismatch { .. }),
            "got: {err}"
        );
        assert!(
            err.to_string().contains("Restart"),
            "say how to fix it: {err}"
        );
    }

    #[tokio::test]
    async fn streams_events_before_the_response() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = socket_path(&dir);
        let listener = UnixListener::bind(&path).expect("bind");

        let id = RequestId(1);
        tokio::spawn(serve(
            listener,
            vec![vec![
                ServerMessage::event(id, Event::step_started("pull", "pulling")),
                ServerMessage::event(id, Event::info("in progress")),
                ServerMessage::event(id, Event::step_done("pull", "pulling")),
                ServerMessage::ok(id, Response::Empty),
            ]],
        ));

        let mut connection = Client::new(path).connect().await.expect("connects");

        let mut events = Vec::new();
        let response = connection
            .call(Request::Ping, |event| events.push(event))
            .await
            .expect("succeeds");

        assert!(matches!(response, Response::Empty));
        assert_eq!(events.len(), 3, "events arrive in order");
    }

    #[tokio::test]
    async fn ignores_events_for_other_requests() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = socket_path(&dir);
        let listener = UnixListener::bind(&path).expect("bind");

        tokio::spawn(serve(
            listener,
            vec![vec![
                // Events for another request must not be mistaken for ours.
                ServerMessage::event(RequestId(99), Event::info("someone else's work")),
                ServerMessage::event(RequestId(1), Event::info("our work")),
                ServerMessage::ok(RequestId(1), Response::Empty),
            ]],
        ));

        let mut connection = Client::new(path).connect().await.expect("connects");

        let mut events = Vec::new();
        connection
            .call(Request::Ping, |event| events.push(event))
            .await
            .expect("succeeds");

        assert_eq!(events.len(), 1, "only our own events arrive");
    }

    #[tokio::test]
    async fn surfaces_api_errors_with_exit_code() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = socket_path(&dir);
        let listener = UnixListener::bind(&path).expect("bind");

        tokio::spawn(serve(
            listener,
            vec![vec![ServerMessage::Response {
                id: RequestId(1),
                outcome: Outcome::Error {
                    error: ApiError::not_found("no such workspace")
                        .with_hint("run minato ls to check"),
                },
            }]],
        ));

        let mut connection = Client::new(path).connect().await.expect("connects");
        let err = connection.request(Request::Ping).await.unwrap_err();

        assert_eq!(err.exit_code(), ErrorCode::NotFound.exit_code());
        assert_eq!(err.hint(), Some("run minato ls to check"));
    }

    #[tokio::test]
    async fn reports_disconnection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = socket_path(&dir);
        let listener = UnixListener::bind(&path).expect("bind");

        // Close without answering.
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accepts");
            drop(stream);
        });

        let mut connection = Client::new(path).connect().await.expect("connects");
        let err = connection.request(Request::Ping).await.unwrap_err();

        assert!(matches!(err, ClientError::Disconnected), "got: {err}");
    }

    #[tokio::test]
    async fn connect_failure_suggests_starting_the_daemon() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = Client::new(socket_path(&dir)).connect().await.unwrap_err();

        assert!(matches!(err, ClientError::Connect { .. }));
        assert!(err.hint().expect("a hint is present").contains("daemon"));
    }

    #[tokio::test]
    async fn spawn_reports_missing_daemon_binary() {
        let dir = tempfile::tempdir().expect("tempdir");
        let client =
            Client::new(socket_path(&dir)).with_daemon_program(dir.path().join("does-not-exist"));

        let err = client.connect_or_spawn().await.unwrap_err();
        assert!(matches!(err, ClientError::Spawn(_)), "got: {err}");
    }

    #[tokio::test]
    async fn removes_stale_socket_before_spawning() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = socket_path(&dir);

        // Mimic the socket file a crashed daemon leaves behind.
        std::fs::write(&path, b"").expect("creates");

        let client =
            Client::new(path.clone()).with_daemon_program(dir.path().join("does-not-exist"));
        let _ = client.connect_or_spawn().await;

        assert!(!path.exists(), "a dead socket is removed before starting");
    }
}
