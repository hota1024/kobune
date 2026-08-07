//! Framing over the Unix socket.
//!
//! One JSON message per line (JSONL). Nothing binary needs to travel here,
//! and being able to debug by hand with `socat` is worth a lot.

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("connection I/O failed: {0}")]
    Io(#[from] std::io::Error),

    #[error("cannot decode the message: {source}\nreceived: {line}")]
    Decode {
        line: String,
        #[source]
        source: serde_json::Error,
    },
}

/// Writes one message as a line and flushes.
///
/// `serde_json::to_string` never emits a newline, so the framing holds.
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

/// Reads line-delimited messages.
///
/// No line-length limit: the only peer is a local Unix socket and the
/// sender is us. Exposing this over a network would need one.
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

    /// Reads the next message, or `None` once the peer closes.
    ///
    /// Blank lines are skipped.
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
        .expect("writes");

        assert!(buffer.ends_with(b"\n"), "ends with the line separator");

        let mut stream = MessageStream::new(buffer.as_slice());
        let message: ClientMessage = stream
            .recv()
            .await
            .expect("reads")
            .expect("a message is present");

        match message {
            ClientMessage::Request { id, request } => {
                assert_eq!(id, RequestId(1));
                assert!(matches!(request, Request::Ping));
            }
            other => panic!("unexpected: {other:?}"),
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
            .expect("writes");
        }

        let mut stream = MessageStream::new(buffer.as_slice());
        for expected in 1..=3 {
            let message: ClientMessage = stream
                .recv()
                .await
                .expect("reads")
                .expect("a message is present");
            match message {
                ClientMessage::Request { id, .. } => assert_eq!(id, RequestId(expected)),
                other => panic!("unexpected: {other:?}"),
            }
        }

        let end: Option<ClientMessage> = stream.recv().await.expect("reads");
        assert!(end.is_none(), "the end of the stream yields None");
    }

    #[tokio::test]
    async fn returns_none_on_empty_input() {
        let mut stream = MessageStream::new(&[][..]);
        let message: Option<ClientMessage> = stream.recv().await.expect("reads");
        assert!(message.is_none());
    }

    #[tokio::test]
    async fn skips_blank_lines() {
        let input = b"\n\n{\"kind\":\"cancel\",\"id\":5}\n";
        let mut stream = MessageStream::new(&input[..]);

        let message: ClientMessage = stream.recv().await.expect("reads").expect("present");
        match message {
            ClientMessage::Cancel { id } => assert_eq!(id, RequestId(5)),
            other => panic!("unexpected: {other:?}"),
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
            "include the offending line so it can be debugged: {text}"
        );
    }
}
