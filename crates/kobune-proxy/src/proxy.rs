//! The reverse proxy itself.
//!
//! Upstream connections are made per request. There is no pool: it keeps
//! WebSocket upgrades straightforward, and connection count is not a
//! concern in a development environment.
//!
//! **WebSocket and SSE must always get through.** Dev-server HMR depends on
//! them, and Kobune is useless if they do not work.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::body::{Body as _, Incoming};
use hyper::header::{HOST, HeaderValue};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;

use crate::activator::{Activation, Activator};
use crate::routes::{Routes, normalize_host};

/// How long to keep trying to reach an upstream.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub type ProxyBody = BoxBody<Bytes, hyper::Error>;

/// How long a browser navigation waits before it gets the waiting page.
///
/// A warm container comes up within this. Flashing a waiting page that
/// vanishes immediately is worse than a short pause.
const BROWSER_GRACE: Duration = Duration::from_millis(1500);

/// How long everything else (curl, fetch, an agent) waits.
///
/// **No half-hearted errors.** A 503 during startup reads to an agent as
/// "the server is broken".
const CLIENT_WAIT: Duration = Duration::from_secs(120);

/// How often the waiting page reloads itself, in seconds.
const RETRY_AFTER_SECS: u32 = 2;

/// How many times the request that woke a service may be sent.
///
/// See [`forward_after_wake`].
const COLD_START_ATTEMPTS: u32 = 4;

/// How long to leave between those attempts.
const COLD_START_BACKOFF: Duration = Duration::from_millis(250);

/// Handles one request.
///
/// No route is a 404; an unreachable upstream is a 502. Both explain
/// themselves in the body — a blank page in the browser or an empty
/// response to an agent gives neither of them anything to go on.
pub async fn handle(
    request: Request<Incoming>,
    routes: Routes,
    activator: Arc<dyn Activator>,
) -> Response<ProxyBody> {
    let raw_host = request
        .headers()
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        // HTTP/2 uses :authority.
        .or_else(|| request.uri().host().map(str::to_string))
        .unwrap_or_default();

    // **Always normalise first.** `Host` can carry a port, and passing it
    // raw to the activator makes the idle-tracking key disagree with the
    // routing key, which is normalised. Accesses then stop counting as
    // accesses, and a service in active use gets shut down.
    let Some(host) = normalize_host(&raw_host) else {
        return error_response(
            StatusCode::BAD_REQUEST,
            format!("Kobune: cannot make sense of the Host header `{raw_host}`.\n"),
        );
    };

    let Some(route) = routes.get(&host) else {
        tracing::debug!("no route for {host}");
        return error_response(
            StatusCode::NOT_FOUND,
            format!(
                "Kobune: there is no environment behind `{host}`.\n\
                 Run `kobune ls` to see which workspaces are up.\n"
            ),
        );
    };

    // Already up: forward by the shortest path. Almost every request
    // goes this way.
    let (endpoint, woken) = match route.endpoint {
        Some(endpoint) => {
            activator.touch(&host);
            (endpoint, false)
        }
        None => match wake(&host, &request, activator.as_ref()).await {
            Ok(endpoint) => (endpoint, true),
            Err(response) => return *response,
        },
    };

    let forwarded = if woken {
        forward_after_wake(request, endpoint).await
    } else {
        forward(request, endpoint).await
    };

    match forwarded {
        Ok(response) => response,
        Err(err) => {
            tracing::debug!("cannot forward to {endpoint}: {err}");
            error_response(
                StatusCode::BAD_GATEWAY,
                format!(
                    "Kobune: cannot forward to {} of {}/{} ({endpoint}).\n\
                     Run `kobune status` to check whether the service is up.\n\
                     Details: {err}\n",
                    route.service, route.project, route.workspace
                ),
            )
        }
    }
}

/// Wakes a stopped service.
///
/// How long to wait depends on the client: a browser gets the waiting page
/// and reloads itself, everything else waits for readiness.
///
/// **The answer is boxed rather than returned flat.** A `Response` is a
/// hundred and twenty-eight bytes, and a `Result` is as wide as its widest
/// arm — so every wake that succeeded, which is nearly all of them, carried
/// the failure's weight home with it. The allocation lands only where a
/// response is being built, which is already allocating a body.
async fn wake(
    host: &str,
    request: &Request<Incoming>,
    activator: &dyn Activator,
) -> Result<SocketAddr, Box<Response<ProxyBody>>> {
    let browser = wants_html(request);
    let wait = if browser { BROWSER_GRACE } else { CLIENT_WAIT };

    match activator.ensure_ready(host, wait).await {
        Activation::Ready(endpoint) => {
            activator.touch(host);
            Ok(endpoint)
        }
        Activation::Starting => Err(Box::new(if browser {
            starting_page(host)
        } else {
            // Only reached once CLIENT_WAIT has run out.
            error_response(
                StatusCode::GATEWAY_TIMEOUT,
                format!(
                    "Kobune: `{host}` did not come up within {} seconds.\n\
                     Check the state with `kobune status` and the cause with `kobune logs`.\n",
                    CLIENT_WAIT.as_secs()
                ),
            )
        })),
        Activation::Unknown => Err(Box::new(error_response(
            StatusCode::NOT_FOUND,
            format!("Kobune: there is no environment behind `{host}`.\n"),
        ))),
        Activation::Failed(reason) => Err(Box::new(error_response(
            StatusCode::BAD_GATEWAY,
            format!(
                "Kobune: cannot start `{host}`.\n\
                 {reason}\n\
                 Run `kobune logs` for the details.\n"
            ),
        ))),
    }
}

/// Whether this is a browser navigation.
///
/// Decided by `text/html` in `Accept`. API calls and an agent's curl do
/// not send it.
fn wants_html(request: &Request<Incoming>) -> bool {
    request
        .headers()
        .get(hyper::header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|accept| accept.contains("text/html"))
}

/// The page that says "starting" and reloads itself.
fn starting_page(host: &str) -> Response<ProxyBody> {
    let body = format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta http-equiv="refresh" content="{RETRY_AFTER_SECS}">
<title>Starting — {host}</title>
<style>
  body {{ font-family: ui-sans-serif, system-ui, sans-serif; display: grid;
         place-items: center; min-height: 100vh; margin: 0; color: #333; }}
  main {{ text-align: center; }}
  code {{ background: #f4f4f5; padding: .2em .4em; border-radius: .25em; }}
  .spinner {{ width: 2rem; height: 2rem; margin: 0 auto 1.5rem;
             border: 3px solid #e4e4e7; border-top-color: #71717a;
             border-radius: 50%; animation: spin 800ms linear infinite; }}
  @keyframes spin {{ to {{ transform: rotate(360deg); }} }}
  @media (prefers-color-scheme: dark) {{
    body {{ background: #18181b; color: #e4e4e7; }}
    code {{ background: #27272a; }}
    .spinner {{ border-color: #3f3f46; border-top-color: #a1a1aa; }}
  }}
</style>
</head>
<body>
<main>
  <div class="spinner"></div>
  <p>Starting <code>{host}</code></p>
  <p>This page reloads itself once it is ready</p>
</main>
</body>
</html>
"#
    );

    Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header(hyper::header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(hyper::header::RETRY_AFTER, RETRY_AFTER_SECS.to_string())
        // A cached "starting" response would keep showing up after the
        // service is live.
        .header(hyper::header::CACHE_CONTROL, "no-store")
        .body(
            Full::new(Bytes::from(body))
                .map_err(|never| match never {})
                .boxed(),
        )
        .expect("a fixed response always builds")
}

/// Forwards the request that woke the service, retrying while it comes up.
///
/// Readiness is decided just before this, so the gap is small — but it is
/// not zero, and a service that binds its port a moment late turns the
/// whole cold start into one 502 for whoever asked for it. Trying again
/// costs a few hundred milliseconds on a path that has already waited.
///
/// **Only a request that can be sent twice is retried.** A body is a
/// stream and is gone once it has been read; a POST would be a second
/// write upstream even if it could be replayed; and an upgrade carries a
/// handle to the client connection that belongs to one attempt. What is
/// left — a bodyless GET — is exactly what a browser navigation and an
/// agent's `curl` send, which is what wakes a service in the first place.
async fn forward_after_wake(
    request: Request<Incoming>,
    upstream: SocketAddr,
) -> Result<Response<ProxyBody>, ProxyError> {
    if !is_replayable(&request) {
        return forward(request, upstream).await;
    }

    let (parts, _) = request.into_parts();

    for attempt in 1..COLD_START_ATTEMPTS {
        match forward(Request::from_parts(parts.clone(), empty_body()), upstream).await {
            Ok(response) => return Ok(response),
            Err(err) => {
                tracing::debug!(
                    "{upstream} is not answering yet on attempt {attempt}: {err}. Trying again"
                );
                tokio::time::sleep(COLD_START_BACKOFF).await;
            }
        }
    }

    // The last attempt reports its own error, so a failure that outlives
    // the retries reads the same as one without them.
    forward(Request::from_parts(parts, empty_body()), upstream).await
}

/// Whether sending this request again would be both possible and harmless.
fn is_replayable(request: &Request<Incoming>) -> bool {
    request.body().is_end_stream()
        && !request.headers().contains_key(hyper::header::UPGRADE)
        && matches!(
            *request.method(),
            Method::GET | Method::HEAD | Method::OPTIONS
        )
}

/// Forwards to the upstream, upgrades (WebSocket) included.
async fn forward<B>(
    mut request: Request<B>,
    upstream: SocketAddr,
) -> Result<Response<ProxyBody>, ProxyError>
where
    B: hyper::body::Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect(upstream))
        .await
        .map_err(|_| ProxyError::ConnectTimeout(upstream))?
        .map_err(|source| ProxyError::Connect { upstream, source })?;

    // Turn Nagle off, so HMR's small messages are not delayed.
    let _ = stream.set_nodelay(true);

    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(ProxyError::Handshake)?;

    // An upgrade needs ownership of the connection, so drive it with
    // `with_upgrades`.
    let connection = connection.with_upgrades();
    let connection_task = tokio::spawn(async move {
        if let Err(err) = connection.await {
            tracing::trace!("upstream connection closed: {err}");
        }
    });

    // Check for an upgrade before forwarding — the request is gone after.
    let upgrade_requested = request.headers().get(hyper::header::UPGRADE).is_some();

    // Host is not rewritten to what the upstream would see. Vite and
    // friends check Host against an allowlist, and keeping the original
    // means the app sees the same URL the browser opened. A request
    // without a Host does get one.
    if !request.headers().contains_key(HOST)
        && let Ok(value) = HeaderValue::from_str(&upstream.to_string())
    {
        request.headers_mut().insert(HOST, value);
    }

    // For an upgrade, take the handle off the original request first.
    let client_upgrade = if upgrade_requested {
        Some(hyper::upgrade::on(&mut request))
    } else {
        None
    };

    let response = sender
        .send_request(request)
        .await
        .map_err(ProxyError::Upstream)?;

    if response.status() == StatusCode::SWITCHING_PROTOCOLS {
        return Ok(splice_upgrade(response, client_upgrade, connection_task).await);
    }

    // An ordinary response streams straight through. The connection stays
    // alive until the body is done.
    tokio::spawn(async move {
        let _ = connection_task.await;
    });

    Ok(response.map(|body| body.boxed()))
}

/// On a 101, joins both upgrades and copies in both directions.
async fn splice_upgrade(
    response: Response<Incoming>,
    client_upgrade: Option<hyper::upgrade::OnUpgrade>,
    connection_task: tokio::task::JoinHandle<()>,
) -> Response<ProxyBody> {
    let (parts, body) = response.into_parts();
    let upstream_upgrade = hyper::upgrade::on(Response::from_parts(parts.clone(), body));

    let Some(client_upgrade) = client_upgrade else {
        // A 101 without an upgrade request means the upstream is broken.
        connection_task.abort();
        return error_response(
            StatusCode::BAD_GATEWAY,
            "Kobune: the upstream returned 101 without an upgrade request\n".to_string(),
        );
    };

    tokio::spawn(async move {
        let (client, upstream) = match tokio::try_join!(client_upgrade, upstream_upgrade) {
            Ok(pair) => pair,
            Err(err) => {
                tracing::debug!("cannot establish the upgrade: {err}");
                connection_task.abort();
                return;
            }
        };

        let mut client = TokioIo::new(client);
        let mut upstream = TokioIo::new(upstream);

        // WebSocket data flows either way. When one side closes, so does
        // the other.
        match tokio::io::copy_bidirectional(&mut client, &mut upstream).await {
            Ok((to_upstream, to_client)) => {
                tracing::trace!("upgrade finished: ↑{to_upstream}B ↓{to_client}B");
            }
            Err(err) => tracing::trace!("upgrade relay finished: {err}"),
        }

        connection_task.abort();
    });

    Response::from_parts(parts, empty_body())
}

fn error_response(status: StatusCode, message: String) -> Response<ProxyBody> {
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(
            Full::new(Bytes::from(message))
                .map_err(|never| match never {})
                .boxed(),
        )
        .expect("a fixed response always builds")
}

fn empty_body() -> ProxyBody {
    Full::new(Bytes::new())
        .map_err(|never| match never {})
        .boxed()
}

#[derive(Debug, thiserror::Error)]
enum ProxyError {
    #[error("timed out connecting to {0}")]
    ConnectTimeout(SocketAddr),

    #[error("cannot connect to {upstream}: {source}")]
    Connect {
        upstream: SocketAddr,
        #[source]
        source: std::io::Error,
    },

    #[error("handshake with the upstream failed: {0}")]
    Handshake(#[source] hyper::Error),

    #[error("the upstream returned an error: {0}")]
    Upstream(#[source] hyper::Error),
}
