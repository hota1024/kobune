//! Unix socket 上のフレーミング。
//!
//! 1 メッセージ 1 行の JSON（JSONL）。バイナリを運ぶ必要がなく、
//! `socat` などで手動デバッグできる利点が大きいためこの形式にする。

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("接続の入出力に失敗しました: {0}")]
    Io(#[from] std::io::Error),

    #[error("メッセージを解釈できません: {source}\n受信した行: {line}")]
    Decode {
        line: String,
        #[source]
        source: serde_json::Error,
    },
}

/// メッセージを 1 行書き出して flush する。
///
/// `serde_json::to_string` は改行を含まないため、行の区切りが壊れることはない。
pub async fn write_message<W, T>(writer: &mut W, message: &T) -> Result<(), CodecError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let mut line = serde_json::to_string(message).map_err(|source| CodecError::Decode {
        line: String::new(),
        source,
    })?;
    line.push('\n');

    writer.write_all(line.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

/// 行区切りのメッセージを読み出す。
///
/// 行長の上限は設けていない。ローカルの Unix socket のみを相手にし、
/// 送り手も自分自身であるため。ネットワーク越しに開く場合はここに上限が要る。
pub struct MessageStream<R> {
    reader: BufReader<R>,
    line: String,
}

impl<R: AsyncRead + Unpin> MessageStream<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader: BufReader::new(reader),
            line: String::new(),
        }
    }

    /// 次のメッセージを読む。接続が閉じられた場合は `None`。
    ///
    /// 空行は読み飛ばす。
    pub async fn recv<T: DeserializeOwned>(&mut self) -> Result<Option<T>, CodecError> {
        loop {
            self.line.clear();
            let read = self.reader.read_line(&mut self.line).await?;
            if read == 0 {
                return Ok(None);
            }

            let trimmed = self.line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let message = serde_json::from_str(trimmed).map_err(|source| CodecError::Decode {
                line: trimmed.to_string(),
                source,
            })?;
            return Ok(Some(message));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ClientMessage, RequestId};
    use crate::request::Request;

    #[tokio::test]
    async fn writes_and_reads_back() {
        let mut buffer = Vec::new();
        write_message(
            &mut buffer,
            &ClientMessage::Request {
                id: RequestId(1),
                request: Request::Ping,
            },
        )
        .await
        .expect("書ける");

        assert!(buffer.ends_with(b"\n"), "行区切りで終わる");

        let mut stream = MessageStream::new(buffer.as_slice());
        let message: ClientMessage = stream
            .recv()
            .await
            .expect("読める")
            .expect("メッセージがある");

        match message {
            ClientMessage::Request { id, request } => {
                assert_eq!(id, RequestId(1));
                assert!(matches!(request, Request::Ping));
            }
            other => panic!("想定外: {other:?}"),
        }
    }

    #[tokio::test]
    async fn reads_multiple_messages_in_order() {
        let mut buffer = Vec::new();
        for n in 1..=3 {
            write_message(
                &mut buffer,
                &ClientMessage::Request {
                    id: RequestId(n),
                    request: Request::Ping,
                },
            )
            .await
            .expect("書ける");
        }

        let mut stream = MessageStream::new(buffer.as_slice());
        for expected in 1..=3 {
            let message: ClientMessage = stream
                .recv()
                .await
                .expect("読める")
                .expect("メッセージがある");
            match message {
                ClientMessage::Request { id, .. } => assert_eq!(id, RequestId(expected)),
                other => panic!("想定外: {other:?}"),
            }
        }

        let end: Option<ClientMessage> = stream.recv().await.expect("読める");
        assert!(end.is_none(), "終端では None を返す");
    }

    #[tokio::test]
    async fn returns_none_on_empty_input() {
        let mut stream = MessageStream::new(&[][..]);
        let message: Option<ClientMessage> = stream.recv().await.expect("読める");
        assert!(message.is_none());
    }

    #[tokio::test]
    async fn skips_blank_lines() {
        let input = b"\n\n{\"kind\":\"cancel\",\"id\":5}\n";
        let mut stream = MessageStream::new(&input[..]);

        let message: ClientMessage = stream.recv().await.expect("読める").expect("ある");
        match message {
            ClientMessage::Cancel { id } => assert_eq!(id, RequestId(5)),
            other => panic!("想定外: {other:?}"),
        }
    }

    #[tokio::test]
    async fn reports_undecodable_line_with_content() {
        let input = b"{not json}\n";
        let mut stream = MessageStream::new(&input[..]);

        let err = stream.recv::<ClientMessage>().await.unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains("{not json}"),
            "デバッグできるよう問題の行を含める: {text}"
        );
    }
}
