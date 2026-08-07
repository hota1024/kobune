//! Unix socket の待ち受けと、1 接続あたりのメッセージ処理。

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

    /// 待ち受けを開始し、停止が指示されるまで接続を受け続ける。
    pub async fn run(&self) -> anyhow::Result<()> {
        let listener = bind(&self.socket)?;
        tracing::info!("待ち受けを開始しました: {}", self.socket.display());

        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    match accepted {
                        Ok((stream, _)) => {
                            let supervisor = self.supervisor.clone();
                            tokio::spawn(async move {
                                if let Err(err) = handle_connection(stream, supervisor).await {
                                    tracing::debug!("接続の処理を終了しました: {err}");
                                }
                            });
                        }
                        Err(err) => {
                            tracing::warn!("接続の受け入れに失敗しました: {err}");
                        }
                    }
                }
                _ = self.shutdown.notified() => {
                    tracing::info!("停止が指示されました");
                    break;
                }
            }
        }

        // 次回の起動が bind できるよう後片付けする。
        let _ = std::fs::remove_file(&self.socket);
        Ok(())
    }
}

/// socket を作る。前回の残骸があれば取り除く。
fn bind(socket: &Path) -> anyhow::Result<UnixListener> {
    if socket.exists() {
        // 生きた daemon がいるなら接続できる。その場合は多重起動なので譲る。
        if std::os::unix::net::UnixStream::connect(socket).is_ok() {
            anyhow::bail!("既に daemon が {} で動いています", socket.display());
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

    // イベントを流すタスクと最終応答が同じ writer を使うため共有する。
    let writer = Arc::new(Mutex::new(write_half));

    while let Some(message) = reader.recv::<ClientMessage>().await? {
        match message {
            ClientMessage::Request { id, request } => {
                let (sink, mut receiver) = EventSink::channel();

                // 処理中のイベントを随時書き出す。
                // 応答より先にすべて流し終える必要があるので、後で join する。
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

                // sink を落とすとチャネルが閉じ、pump が終わる。
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
                // M0 では処理を中断できない。無視すると呼び出し側が待ち続けるため、
                // 中断できなかったことをログに残す。
                tracing::debug!("リクエスト {id} の中断は M0 では未対応です");
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
        assert!(err.to_string().contains("既に daemon"), "got: {err}");
    }

    #[tokio::test]
    async fn replaces_a_stale_socket_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("minatod.sock");

        // 誰も listen していない残骸。
        std::fs::write(&socket, b"").expect("作れる");

        let listener = bind(&socket).expect("残骸を消して bind できる");
        drop(listener);
    }

    #[tokio::test]
    async fn creates_the_parent_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("nested").join("minatod.sock");

        let listener = bind(&socket).expect("親ごと作る");
        assert!(socket.exists());
        drop(listener);
    }
}
