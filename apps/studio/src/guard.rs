//! What stands in for the socket's uid check once there is a TCP port.
//!
//! The daemon's socket asks for nothing because the only way to reach it
//! is to be the user who owns `KOBUNE_HOME` (`docs/DESIGN.md` §3). A
//! listening port has no such answer: every process on the machine can
//! connect to loopback, and so can every page the browser happens to have
//! open. Three things replace it, and a request has to pass all three.
//!
//! - **A token, minted per session**, required as `Authorization: Bearer`
//!   on every `/api` request. It is never written to a file and does not
//!   outlive the process
//! - **The `Authorization` header itself.** It is not a CORS-simple
//!   header, so a cross-origin request carrying one is preflighted — and
//!   this server answers no preflight. A page on the open web therefore
//!   cannot call the API even if it guesses the port
//! - **`Host` and `Origin` are checked**, which is what stops DNS
//!   rebinding: a name that resolves to 127.0.0.1 to get past the browser
//!   still arrives with its own name in `Host`
//!
//! The static shell is served without a token — it is an empty page, and
//! the alternative is a chicken and egg. Everything that reaches the
//! daemon is behind all three.

use std::net::SocketAddr;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;
use axum::{body::Body, extract::Request};

/// One run of the dashboard: where it listens, and the secret that lets a
/// page talk to it.
#[derive(Clone)]
pub struct Session {
    addr: SocketAddr,
    token: String,
}

impl Session {
    /// Mints a session token.
    ///
    /// From the kernel's pool rather than a seeded generator: this is the
    /// only thing between a local process and `exec -- env`, and a
    /// predictable one would be no barrier at all. 32 bytes, hex — long
    /// enough that guessing is not a strategy, short enough to sit in a
    /// URL.
    pub fn new(addr: SocketAddr) -> anyhow::Result<Self> {
        use std::io::Read as _;

        let mut bytes = [0u8; 32];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;

        let mut token = String::with_capacity(64);
        for byte in bytes {
            use std::fmt::Write as _;
            let _ = write!(token, "{byte:02x}");
        }

        Ok(Self { addr, token })
    }

    /// The URL to open, with the token in the fragment.
    ///
    /// The workspace rides along beside the token rather than as a query
    /// parameter, for the same reason the token is in the fragment: it is
    /// not sent to the server, so neither shows up in its own request log.
    /// It also keeps `/api/state` describing the environment alone — which
    /// workspace this particular browser should open first is not a fact
    /// about the environment.
    pub fn entry_url(&self, workspace: Option<&str>) -> String {
        let mut url = format!(
            "http://127.0.0.1:{}/#token={}",
            self.addr.port(),
            self.token
        );

        if let Some(workspace) = workspace {
            url.push_str("&workspace=");
            url.push_str(&percent_encode(workspace));
        }

        url
    }

    fn port(&self) -> u16 {
        self.addr.port()
    }

    /// Compares in constant time.
    ///
    /// The comparison is against a secret, and `==` on `str` stops at the
    /// first byte that differs. Timing a few thousand requests over
    /// loopback is precise enough for that to matter.
    fn token_matches(&self, presented: &str) -> bool {
        let expected = self.token.as_bytes();
        let presented = presented.as_bytes();
        if expected.len() != presented.len() {
            return false;
        }
        let mut diff = 0u8;
        for (a, b) in expected.iter().zip(presented) {
            diff |= a ^ b;
        }
        diff == 0
    }

    /// Whether a `Host` or `Origin` names this server rather than one that
    /// merely resolves to it.
    fn is_ours(&self, authority: &str) -> bool {
        let authority = authority
            .trim_start_matches("http://")
            .trim_start_matches("https://");
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (host, port.parse::<u16>().ok()),
            None => (authority, None),
        };

        // A missing port would mean 80, which is never this server.
        if port != Some(self.port()) {
            return false;
        }

        matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1")
    }
}

/// Rejects anything that has not proved it is the page this session opened.
pub async fn require_session(
    State(session): State<Session>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Response {
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    let Some(host) = host else {
        return refuse("no Host header");
    };
    if !session.is_ours(host) {
        return refuse("the Host header names somewhere else");
    }

    // Absent on a same-origin GET, which is why it is only checked when it
    // is there. A cross-origin request that reaches this far always has one.
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok())
        && !session.is_ours(origin)
    {
        return refuse("the Origin header names somewhere else");
    }

    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    match presented {
        Some(token) if session.token_matches(token) => next.run(request).await,
        _ => refuse("no session token"),
    }
}

/// Escapes what a URL fragment cannot carry literally.
///
/// A workspace label is sanitised before it becomes a hostname
/// (`docs/DESIGN.md` §5), so in practice it is already safe. This is here
/// because `--workspace` takes whatever was typed, and a `#` or an `&` in
/// it would otherwise cut the fragment in half.
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());

    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }

    out
}

/// Says no without saying what is here.
///
/// The daemon drops a connection from the wrong uid rather than answering
/// it, for the reason in §3. The equivalent here is a bare 401: a body
/// naming the project, or distinguishing "wrong token" from "wrong
/// origin", would tell somebody who has proved nothing.
fn refuse(why: &str) -> Response {
    tracing::warn!("refused a request: {why}");
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .body(Body::empty())
        .expect("a bare 401 is always well formed")
}
