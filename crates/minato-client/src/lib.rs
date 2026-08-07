//! daemon への接続。CLI と GUI が共有する。
//!
//! この crate は `minato-runtime` に依存しない。クライアントが runtime を
//! 直接触れてしまうと「daemon の API が製品の本体」という原則が崩れる
//! （`docs/DESIGN.md` §3, §13）。

use std::path::{Path, PathBuf};
use std::time::Duration;

use minato_api::{
    ApiError, ClientMessage, Event, MessageStream, PROTOCOL_VERSION, Pong, Request, RequestId,
    Response, ServerMessage, write_message,
};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

/// daemon の起動を待つ最大時間。
const SPAWN_TIMEOUT: Duration = Duration::from_secs(10);

/// 起動待ちのポーリング間隔。
const SPAWN_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// daemon の実行ファイル名。
const DAEMON_PROGRAM: &str = "minatod";

/// daemon の場所を明示するための環境変数。
pub const DAEMON_ENV: &str = "MINATO_DAEMON";

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("daemon に接続できません ({path}): {source}")]
    Connect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("daemon を起動できません: {0}")]
    Spawn(String),

    #[error("daemon の起動を {}秒 待ちましたが応答がありません", SPAWN_TIMEOUT.as_secs())]
    SpawnTimeout,

    #[error("daemon との通信が切断されました")]
    Disconnected,

    #[error(transparent)]
    Codec(#[from] minato_api::CodecError),

    /// daemon が処理を拒否した。呼び出し側はこれを表示すればよい。
    #[error("{0}")]
    Api(#[source] ApiError),

    #[error("daemon が想定外の応答を返しました: {0}")]
    Protocol(String),

    #[error(
        "daemon のプロトコル版 {server} は、この minato (版 {client}) と互換性がありません。\
         `minato daemon restart` で daemon を再起動してください"
    )]
    VersionMismatch { client: u32, server: u32 },
}

impl ClientError {
    /// CLI がプロセス終了コードとして返す値。
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Api(err) => err.code.exit_code(),
            _ => 1,
        }
    }

    /// 利用者に見せる対処方法。
    pub fn hint(&self) -> Option<&str> {
        match self {
            Self::Api(err) => err.hint.as_deref(),
            Self::Connect { .. } => Some("`minato daemon start` で daemon を起動してください"),
            _ => None,
        }
    }
}

/// daemon への接続を作る。
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

    /// 既定のパス（`$MINATO_HOME` または `~/.minato`）から作る。
    ///
    /// socket のパス長もここで確かめる。長すぎる場合、daemon は起動直後に
    /// 落ちるだけで、クライアント側からは「応答がない」としか見えないため。
    pub fn from_env() -> minato_core::Result<Self> {
        let paths = minato_core::Paths::resolve()?;
        paths.check_socket_length()?;
        Ok(Self::new(paths.socket()))
    }

    /// daemon の実行ファイルを明示する。省略時は環境変数と PATH から探す。
    pub fn with_daemon_program(mut self, program: PathBuf) -> Self {
        self.daemon_program = Some(program);
        self
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    /// 既に動いている daemon に繋ぐ。
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

    /// 繋がらなければ daemon を起動してから繋ぐ。
    ///
    /// CLI は基本これを使う。利用者に daemon の存在を意識させないため。
    pub async fn connect_or_spawn(&self) -> Result<Connection, ClientError> {
        match self.connect().await {
            Ok(connection) => return Ok(connection),
            Err(err) => {
                tracing::debug!("daemon への接続に失敗、起動を試みます: {err}");
            }
        }

        // daemon が異常終了すると socket ファイルだけが残る。
        // 残骸があると bind に失敗するので、繋がらないものは消しておく。
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

        // 親（CLI）が終了しても daemon は動き続ける必要がある。
        // 標準入出力を切り離し、ログは daemon 自身がファイルへ書く。
        std::process::Command::new(&program)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|err| {
                ClientError::Spawn(format!("{} の起動に失敗しました: {err}", program.display()))
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

        // CLI と daemon は一緒に配布されるので、隣にいる可能性が高い。
        // PATH より先に見ることで、開発中の target/debug のものが確実に使われる。
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

/// 確立済みの接続。
///
/// 1 本の接続でリクエストを順に処理する。並行に投げる必要が出たら
/// プロトコル側は既に多重化に対応しているので、ここを拡張すればよい。
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

    /// リクエストを送り、届いたイベントを `on_event` に渡しながら応答を待つ。
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
                    // 他のリクエストのイベントは呼び出し側に渡さない。
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

    /// イベントを捨てて応答だけ受け取る。
    pub async fn request(&mut self, request: Request) -> Result<Response, ClientError> {
        self.call(request, |_| {}).await
    }

    /// 疎通確認とプロトコル版の照合。
    pub async fn handshake(&mut self) -> Result<Pong, ClientError> {
        let response = self.request(Request::Ping).await?;

        let Response::Pong(pong) = response else {
            return Err(ClientError::Protocol(
                "Ping に対して Pong 以外が返りました".to_string(),
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

    /// 決められた応答を返すだけの daemon 代役。
    async fn serve(listener: UnixListener, responses: Vec<Vec<ServerMessage>>) {
        let (stream, _) = listener.accept().await.expect("接続を受ける");
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = MessageStream::new(read_half);

        for batch in responses {
            let Some(_request): Option<ClientMessage> = reader.recv().await.expect("読める")
            else {
                return;
            };

            for message in batch {
                write_message(&mut write_half, &message)
                    .await
                    .expect("書ける");
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

        let mut connection = Client::new(path).connect().await.expect("接続できる");
        let result = connection.handshake().await.expect("成功する");
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

        let mut connection = Client::new(path).connect().await.expect("接続できる");
        let err = connection.handshake().await.unwrap_err();

        assert!(
            matches!(err, ClientError::VersionMismatch { .. }),
            "got: {err}"
        );
        assert!(err.to_string().contains("再起動"), "対処方法を示す: {err}");
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
                ServerMessage::event(id, Event::step_started("pull", "取得")),
                ServerMessage::event(id, Event::info("進行中")),
                ServerMessage::event(id, Event::step_done("pull", "取得")),
                ServerMessage::ok(id, Response::Empty),
            ]],
        ));

        let mut connection = Client::new(path).connect().await.expect("接続できる");

        let mut events = Vec::new();
        let response = connection
            .call(Request::Ping, |event| events.push(event))
            .await
            .expect("成功する");

        assert!(matches!(response, Response::Empty));
        assert_eq!(events.len(), 3, "イベントが順に届く");
    }

    #[tokio::test]
    async fn ignores_events_for_other_requests() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = socket_path(&dir);
        let listener = UnixListener::bind(&path).expect("bind");

        tokio::spawn(serve(
            listener,
            vec![vec![
                // 別リクエストのイベントが混ざっても取り違えない。
                ServerMessage::event(RequestId(99), Event::info("他の処理")),
                ServerMessage::event(RequestId(1), Event::info("自分の処理")),
                ServerMessage::ok(RequestId(1), Response::Empty),
            ]],
        ));

        let mut connection = Client::new(path).connect().await.expect("接続できる");

        let mut events = Vec::new();
        connection
            .call(Request::Ping, |event| events.push(event))
            .await
            .expect("成功する");

        assert_eq!(events.len(), 1, "自分宛てのイベントだけ受け取る");
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
                    error: ApiError::not_found("workspace がありません")
                        .with_hint("minato ls で確認してください"),
                },
            }]],
        ));

        let mut connection = Client::new(path).connect().await.expect("接続できる");
        let err = connection.request(Request::Ping).await.unwrap_err();

        assert_eq!(err.exit_code(), ErrorCode::NotFound.exit_code());
        assert_eq!(err.hint(), Some("minato ls で確認してください"));
    }

    #[tokio::test]
    async fn reports_disconnection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = socket_path(&dir);
        let listener = UnixListener::bind(&path).expect("bind");

        // 応答せずに切断する。
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("接続を受ける");
            drop(stream);
        });

        let mut connection = Client::new(path).connect().await.expect("接続できる");
        let err = connection.request(Request::Ping).await.unwrap_err();

        assert!(matches!(err, ClientError::Disconnected), "got: {err}");
    }

    #[tokio::test]
    async fn connect_failure_suggests_starting_the_daemon() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = Client::new(socket_path(&dir)).connect().await.unwrap_err();

        assert!(matches!(err, ClientError::Connect { .. }));
        assert!(err.hint().expect("hint がある").contains("daemon"));
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

        // daemon が異常終了したあとに残る socket ファイルを模す。
        std::fs::write(&path, b"").expect("作れる");

        let client =
            Client::new(path.clone()).with_daemon_program(dir.path().join("does-not-exist"));
        let _ = client.connect_or_spawn().await;

        assert!(!path.exists(), "繋がらない socket は消してから起動する");
    }
}
