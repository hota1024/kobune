//! The messages exchanged between a client and the daemon.
//!
//! Every message carries a [`RequestId`] so that several requests can be
//! multiplexed over one connection. A request produces zero or more
//! [`Event`]s and is terminated by exactly one response.

use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::event::Event;
use crate::request::Request;
use crate::response::Response;

/// The number that decides protocol compatibility.
///
/// Bumped on every breaking change. Clients compare it during the initial
/// [`Request::Ping`] and ask for a daemon restart on mismatch.
pub const PROTOCOL_VERSION: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(pub u64);

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClientMessage {
    Request {
        id: RequestId,
        request: Request,
    },
    /// Asks to abort work in progress. The daemon stops what it can and
    /// terminates with [`crate::error::ErrorCode::Cancelled`].
    Cancel {
        id: RequestId,
    },

    /// What the person at the keyboard typed, for an attached request.
    ///
    /// Base64: this is whatever the terminal produced — arrow keys, a
    /// paste, a mouse report — and not necessarily valid UTF-8, while the
    /// framing is one JSON document per line.
    ///
    /// Ignored when the request named by `id` is not attached, rather than
    /// being an error. A key pressed a moment after the program exited is
    /// nobody's mistake.
    Input {
        id: RequestId,
        data: String,
    },

    /// The client's terminal changed size.
    ///
    /// Sent once when attaching too, since the container's terminal starts
    /// at whatever size the runtime picked — 80×24 — and a full-screen UI
    /// drawn to that size in a wider window is the first thing anyone
    /// notices.
    Resize {
        id: RequestId,
        window: crate::request::Window,
    },
}

impl ClientMessage {
    /// Carries typed bytes, encoded for the wire.
    pub fn input(id: RequestId, bytes: &[u8]) -> Self {
        Self::Input {
            id,
            data: crate::event::encode_bytes(bytes),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServerMessage {
    /// An interim report. Any number may arrive.
    Event { id: RequestId, event: Event },

    /// Terminates a request. Sent exactly once per `id`.
    Response {
        id: RequestId,
        #[serde(flatten)]
        outcome: Outcome,
    },

    /// A fatal error not tied to any request, such as a protocol violation.
    /// The daemon closes the connection after sending this.
    Fatal { message: String },
}

impl ServerMessage {
    pub fn ok(id: RequestId, value: Response) -> Self {
        Self::Response {
            id,
            outcome: Outcome::Ok { value },
        }
    }

    pub fn err(id: RequestId, error: ApiError) -> Self {
        Self::Response {
            id,
            outcome: Outcome::Error { error },
        }
    }

    pub fn event(id: RequestId, event: Event) -> Self {
        Self::Event { id, event }
    }

    /// Whether this message terminates its request.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Response { .. } | Self::Fatal { .. })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Ok { value: Response },
    Error { error: ApiError },
}

impl Outcome {
    pub fn into_result(self) -> Result<Response, ApiError> {
        match self {
            Self::Ok { value } => Ok(value),
            Self::Error { error } => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use crate::event::Event;
    use crate::request::Target;
    use crate::response::Pong;
    use std::path::PathBuf;

    #[test]
    fn request_id_is_transparent_on_the_wire() {
        let json = serde_json::to_string(&RequestId(7)).expect("serializes");
        assert_eq!(json, "7", "sent as a bare number, not wrapped");
    }

    #[test]
    fn multiplexes_events_and_response_under_one_id() {
        let id = RequestId(1);
        let messages = vec![
            ServerMessage::event(id, Event::step_started("pull", "pulling")),
            ServerMessage::event(id, Event::step_done("pull", "pulling")),
            ServerMessage::ok(
                id,
                Response::Pong(Pong {
                    version: "0.1.0".into(),
                    protocol: PROTOCOL_VERSION,
                    runtime: "docker".into(),
                    uptime_secs: 3,
                }),
            ),
        ];

        let terminal_count = messages.iter().filter(|m| m.is_terminal()).count();
        assert_eq!(terminal_count, 1, "exactly one terminator");

        for message in &messages {
            let json = serde_json::to_string(message).expect("serializes");
            let back: ServerMessage = serde_json::from_str(&json).expect("deserializes");
            assert_eq!(back.is_terminal(), message.is_terminal());
        }
    }

    #[test]
    fn outcome_flattens_beside_the_id() {
        let message = ServerMessage::err(RequestId(3), ApiError::not_found("no such workspace"));
        let json = serde_json::to_string(&message).expect("serializes");

        assert!(json.contains(r#""kind":"response""#), "got: {json}");
        assert!(json.contains(r#""status":"error""#), "got: {json}");
        assert!(json.contains(r#""id":3"#), "got: {json}");
    }

    #[test]
    fn outcome_converts_to_result() {
        let ok = Outcome::Ok {
            value: Response::Empty,
        };
        assert!(ok.into_result().is_ok());

        let err = Outcome::Error {
            error: ApiError::new(ErrorCode::NotFound, "missing"),
        };
        let error = err.into_result().unwrap_err();
        assert_eq!(error.code, ErrorCode::NotFound);
    }

    #[test]
    fn roundtrips_client_messages() {
        let messages = vec![
            ClientMessage::Request {
                id: RequestId(1),
                request: Request::Ping,
            },
            ClientMessage::Request {
                id: RequestId(2),
                request: Request::Status {
                    target: Target::new(PathBuf::from("/repo")),
                },
            },
            ClientMessage::Cancel { id: RequestId(2) },
            ClientMessage::input(RequestId(3), b"\x1b[B\r"),
            ClientMessage::Resize {
                id: RequestId(3),
                window: crate::request::Window::new(120, 40),
            },
        ];

        for message in messages {
            let json = serde_json::to_string(&message).expect("serializes");
            let _: ClientMessage = serde_json::from_str(&json).expect("deserializes");
            assert!(!json.contains('\n'), "must fit on one line: {json}");
        }
    }
}
