//! Exercises the proxy over real TCP.
//!
//! Forwarding, error responses, WebSocket upgrades and TLS cannot be
//! covered by unit tests. This is the most fragile part of M1, so it is
//! pinned down with real traffic.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Full, combinators::BoxBody};
use hyper::body::Incoming;
use hyper::header::{CONNECTION, HOST, UPGRADE};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use minato_proxy::{
    Activation, Activator, LocalCa, NoopActivator, Route, Routes, serve_http, serve_https,
    server_config,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;

type TestBody = BoxBody<Bytes, hyper::Error>;

fn text(body: &str) -> TestBody {
    Full::new(Bytes::from(body.to_string()))
        .map_err(|never| match never {})
        .boxed()
}

/// An upstream for tests. Reflects the path, and echoes on upgrade.
async fn spawn_upstream() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };

            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let service = service_fn(|mut request: Request<Incoming>| async move {
                    if request.headers().contains_key(UPGRADE) {
                        // After the 101, echo raw bytes back.
                        tokio::spawn(async move {
                            let Ok(upgraded) = hyper::upgrade::on(&mut request).await else {
                                return;
                            };
                            let mut io = TokioIo::new(upgraded);
                            let mut buffer = [0u8; 1024];
                            loop {
                                match io.read(&mut buffer).await {
                                    Ok(0) | Err(_) => return,
                                    Ok(n) => {
                                        if io.write_all(&buffer[..n]).await.is_err() {
                                            return;
                                        }
                                    }
                                }
                            }
                        });

                        return Ok::<_, Infallible>(
                            Response::builder()
                                .status(StatusCode::SWITCHING_PROTOCOLS)
                                .header(UPGRADE, "echo")
                                .header(CONNECTION, "Upgrade")
                                .body(
                                    Empty::<Bytes>::new()
                                        .map_err(|never| match never {})
                                        .boxed(),
                                )
                                .expect("builds"),
                        );
                    }

                    let host = request
                        .headers()
                        .get(HOST)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("(none)")
                        .to_string();

                    Ok::<_, Infallible>(Response::new(text(&format!(
                        "upstream {} host={host}",
                        request.uri().path()
                    ))))
                });

                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, service)
                    .with_upgrades()
                    .await;
            });
        }
    });

    addr
}

/// Starts the proxy and returns the address it listens on.
async fn spawn_proxy(routes: Routes) -> SocketAddr {
    spawn_proxy_with(routes, Arc::new(NoopActivator)).await
}

async fn spawn_proxy_with(routes: Routes, activator: Arc<dyn Activator>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let shutdown = Arc::new(Notify::new());

    tokio::spawn(async move {
        let _ = serve_http(listener, routes, activator, shutdown).await;
    });

    addr
}

/// An activator that counts calls and returns a fixed result.
struct ScriptedActivator {
    result: Activation,
    woken: AtomicUsize,
    touched: AtomicUsize,
    /// The hostnames passed to `touch` and `ensure_ready`.
    seen: std::sync::Mutex<Vec<String>>,
}

impl ScriptedActivator {
    fn new(result: Activation) -> Arc<Self> {
        Arc::new(Self {
            result,
            woken: AtomicUsize::new(0),
            touched: AtomicUsize::new(0),
            seen: std::sync::Mutex::new(Vec::new()),
        })
    }
}

#[async_trait::async_trait]
impl Activator for ScriptedActivator {
    async fn ensure_ready(&self, host: &str, _wait: std::time::Duration) -> Activation {
        self.woken.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().expect("lock").push(host.to_string());
        self.result.clone()
    }

    fn touch(&self, host: &str) {
        self.touched.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().expect("lock").push(host.to_string());
    }
}

/// Requests with `Accept: text/html`, standing in for a browser.
async fn request_as_browser(proxy: SocketAddr, host: &str) -> (StatusCode, String) {
    let stream = TcpStream::connect(proxy).await.expect("connects");
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .expect("handshake");

    tokio::spawn(async move {
        let _ = connection.await;
    });

    let request = Request::builder()
        .uri("/")
        .header(HOST, host)
        .header(hyper::header::ACCEPT, "text/html,application/xhtml+xml")
        .body(
            Empty::<Bytes>::new()
                .map_err(|never| match never {})
                .boxed(),
        )
        .expect("builds");

    let response = sender.send_request(request).await.expect("gets a response");
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("reads the body")
        .to_bytes();

    (status, String::from_utf8_lossy(&body).to_string())
}

/// Sends one request through the proxy and takes the response.
async fn request_through(proxy: SocketAddr, host: &str, path: &str) -> (StatusCode, String) {
    let stream = TcpStream::connect(proxy).await.expect("connects");
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .expect("handshake");

    tokio::spawn(async move {
        let _ = connection.await;
    });

    let request = Request::builder()
        .uri(path)
        .header(HOST, host)
        .body(
            Empty::<Bytes>::new()
                .map_err(|never| match never {})
                .boxed(),
        )
        .expect("builds");

    let response = sender.send_request(request).await.expect("gets a response");
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("reads the body")
        .to_bytes();

    (status, String::from_utf8_lossy(&body).to_string())
}

#[tokio::test]
async fn forwards_to_the_matching_upstream() {
    let upstream = spawn_upstream().await;
    let routes = Routes::new();
    routes.insert(
        "web.feat-1.myapp.localhost",
        Route::new(upstream, "myapp", "feat-1", "web"),
    );

    let proxy = spawn_proxy(routes).await;
    let (status, body) = request_through(proxy, "web.feat-1.myapp.localhost", "/hello").await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("upstream /hello"), "got: {body}");
}

#[tokio::test]
async fn preserves_the_original_host_header() {
    // Vite and friends check Host against an allowlist; rewriting it
    // gets the request rejected.
    let upstream = spawn_upstream().await;
    let routes = Routes::new();
    routes.insert(
        "web.feat-1.myapp.localhost",
        Route::new(upstream, "myapp", "feat-1", "web"),
    );

    let proxy = spawn_proxy(routes).await;
    let (_, body) = request_through(proxy, "web.feat-1.myapp.localhost", "/").await;

    assert!(
        body.contains("host=web.feat-1.myapp.localhost"),
        "the app has to see the same URL the browser opened: {body}"
    );
}

#[tokio::test]
async fn routes_different_hosts_to_different_upstreams() {
    let one = spawn_upstream().await;
    let two = spawn_upstream().await;

    let routes = Routes::new();
    routes.insert(
        "web.a.myapp.localhost",
        Route::new(one, "myapp", "a", "web"),
    );
    routes.insert(
        "web.b.myapp.localhost",
        Route::new(two, "myapp", "b", "web"),
    );

    let proxy = spawn_proxy(routes).await;

    let (_, first) = request_through(proxy, "web.a.myapp.localhost", "/one").await;
    let (_, second) = request_through(proxy, "web.b.myapp.localhost", "/two").await;

    assert!(first.contains("/one"));
    assert!(second.contains("/two"));
}

#[tokio::test]
async fn unknown_host_explains_itself() {
    let proxy = spawn_proxy(Routes::new()).await;
    let (status, body) = request_through(proxy, "nope.myapp.localhost", "/").await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        body.contains("minato ls"),
        "it has to say what to do next: {body}"
    );
}

#[tokio::test]
async fn dead_upstream_returns_bad_gateway() {
    // An address nobody is listening on.
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let dead = listener.local_addr().expect("addr");
    drop(listener);

    let routes = Routes::new();
    routes.insert(
        "web.feat-1.myapp.localhost",
        Route::new(dead, "myapp", "feat-1", "web"),
    );

    let proxy = spawn_proxy(routes).await;
    let (status, body) = request_through(proxy, "web.feat-1.myapp.localhost", "/").await;

    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(
        body.contains("minato status"),
        "it has to give something to go on: {body}"
    );
}

#[tokio::test]
async fn passes_websocket_style_upgrades_through() {
    // HMR depends on this. Without it Minato is useless.
    let upstream = spawn_upstream().await;
    let routes = Routes::new();
    routes.insert(
        "web.feat-1.myapp.localhost",
        Route::new(upstream, "myapp", "feat-1", "web"),
    );

    let proxy = spawn_proxy(routes).await;

    let stream = TcpStream::connect(proxy).await.expect("connects");
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .expect("handshake");

    tokio::spawn(async move {
        let _ = connection.with_upgrades().await;
    });

    let request = Request::builder()
        .uri("/ws")
        .header(HOST, "web.feat-1.myapp.localhost")
        .header(UPGRADE, "echo")
        .header(CONNECTION, "Upgrade")
        .body(
            Empty::<Bytes>::new()
                .map_err(|never| match never {})
                .boxed(),
        )
        .expect("builds");

    let response = sender.send_request(request).await.expect("gets a response");
    assert_eq!(
        response.status(),
        StatusCode::SWITCHING_PROTOCOLS,
        "without a 101 there is no WebSocket"
    );

    let upgraded = hyper::upgrade::on(response).await.expect("upgrades");
    let mut io = TokioIo::new(upgraded);

    io.write_all(b"hello over websocket").await.expect("sends");

    let mut buffer = vec![0u8; 20];
    io.read_exact(&mut buffer).await.expect("comes back");

    assert_eq!(
        &buffer, b"hello over websocket",
        "bytes have to flow both ways"
    );
}

#[tokio::test]
async fn serves_https_with_a_certificate_for_the_sni_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ca = Arc::new(LocalCa::load_or_create(dir.path()).expect("creates a CA"));
    let ca_der = rustls::pki_types::CertificateDer::from(
        rustls_pemfile::certs(&mut std::io::BufReader::new(
            ca.certificate_pem().as_bytes(),
        ))
        .next()
        .expect("has a certificate")
        .expect("reads")
        .to_vec(),
    );

    let upstream = spawn_upstream().await;
    let routes = Routes::new();
    routes.insert(
        "web.feat-1.myapp.localhost",
        Route::new(upstream, "myapp", "feat-1", "web"),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let shutdown = Arc::new(Notify::new());

    tokio::spawn(async move {
        let _ = serve_https(
            listener,
            routes,
            Arc::new(NoopActivator),
            server_config(ca),
            shutdown,
        )
        .await;
    });

    // Connect as a client that trusts only the CA — the same footing as
    // a real browser.
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca_der).expect("trusts the CA");

    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let server_name = rustls::pki_types::ServerName::try_from("web.feat-1.myapp.localhost")
        .expect("a valid name");

    let tcp = TcpStream::connect(addr).await.expect("connects");
    let tls = connector
        .connect(server_name, tcp)
        .await
        .expect("the dynamically issued certificate verifies");

    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
        .await
        .expect("handshake");

    tokio::spawn(async move {
        let _ = connection.await;
    });

    let request = Request::builder()
        .uri("/secure")
        .header(HOST, "web.feat-1.myapp.localhost")
        .body(
            Empty::<Bytes>::new()
                .map_err(|never| match never {})
                .boxed(),
        )
        .expect("builds");

    let response = sender.send_request(request).await.expect("gets a response");
    assert_eq!(response.status(), StatusCode::OK);

    let body = response
        .into_body()
        .collect()
        .await
        .expect("reads the body")
        .to_bytes();

    assert!(String::from_utf8_lossy(&body).contains("upstream /secure"));
}

/// Every name the CA's constraint is supposed to cover, put to a real
/// verifier.
///
/// **This is the test the constraint needed and did not have.** The first
/// version of `PERMITTED_SUFFIXES` carried a leading dot — `.localhost` —
/// on the belief that the dot is what makes a subtree cover what is under
/// it. It is the opposite: RFC 5280 §4.2.1.10 already covers everything
/// with labels prepended, and the dot is a non-standard form meaning
/// *strictly* below, which excludes `localhost` itself. Every unit test
/// passed, because they all asked rcgen's own parser rather than anything
/// that verifies.
///
/// `localhost` is not a hypothetical: `minato-dns` answers for the apex,
/// so `https://localhost` is a URL a person really opens.
#[tokio::test]
async fn the_constrained_ca_verifies_for_every_name_it_covers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ca = Arc::new(LocalCa::load_or_create(dir.path()).expect("creates a CA"));

    let ca_der = rustls::pki_types::CertificateDer::from(
        rustls_pemfile::certs(&mut std::io::BufReader::new(
            ca.certificate_pem().as_bytes(),
        ))
        .next()
        .expect("has a certificate")
        .expect("reads")
        .to_vec(),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let shutdown = Arc::new(Notify::new());

    tokio::spawn(async move {
        let _ = serve_https(
            listener,
            Routes::new(),
            Arc::new(NoopActivator),
            server_config(ca),
            shutdown,
        )
        .await;
    });

    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca_der).expect("trusts the CA");
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));

    for name in [
        // The apex, which a leading dot would exclude.
        "localhost",
        // A project on the main worktree.
        "web.myapp.localhost",
        // And one at the depth every new worktree invents.
        "api.feature-user-auth.myapp.localhost",
    ] {
        let server_name = rustls::pki_types::ServerName::try_from(name).expect("a valid name");
        let tcp = TcpStream::connect(addr).await.expect("connects");

        connector
            .connect(server_name, tcp)
            .await
            .unwrap_or_else(|err| {
                panic!(
                    "a client trusting only this CA must accept {name}, and did not: {err}. \
                     The name constraint is refusing a name Minato serves"
                )
            });
    }
}

#[tokio::test]
async fn wakes_a_stopped_service_and_forwards() {
    // Scale-to-zero itself: a request wakes a stopped service and goes
    // straight through.
    let upstream = spawn_upstream().await;
    let routes = Routes::new();
    routes.insert(
        "web.feat-1.myapp.localhost",
        Route::stopped("myapp", "feat-1", "web"),
    );

    let activator = ScriptedActivator::new(Activation::Ready(upstream));
    let proxy = spawn_proxy_with(routes, activator.clone()).await;

    let (status, body) = request_through(proxy, "web.feat-1.myapp.localhost", "/woken").await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("upstream /woken"), "got: {body}");
    assert_eq!(
        activator.woken.load(Ordering::SeqCst),
        1,
        "asks for a start"
    );
    assert_eq!(
        activator.touched.load(Ordering::SeqCst),
        1,
        "records the access"
    );
}

#[tokio::test]
async fn running_services_are_not_woken() {
    // Calling the activator on every request to a running service would
    // make it the bottleneck.
    let upstream = spawn_upstream().await;
    let routes = Routes::new();
    routes.insert(
        "web.feat-1.myapp.localhost",
        Route::new(upstream, "myapp", "feat-1", "web"),
    );

    let activator = ScriptedActivator::new(Activation::Ready(upstream));
    let proxy = spawn_proxy_with(routes, activator.clone()).await;

    request_through(proxy, "web.feat-1.myapp.localhost", "/").await;

    assert_eq!(activator.woken.load(Ordering::SeqCst), 0, "no start needed");
    assert_eq!(
        activator.touched.load(Ordering::SeqCst),
        1,
        "idle detection still needs the record"
    );
}

#[tokio::test]
async fn browsers_get_a_self_refreshing_page_while_starting() {
    let routes = Routes::new();
    routes.insert(
        "web.feat-1.myapp.localhost",
        Route::stopped("myapp", "feat-1", "web"),
    );

    let activator = ScriptedActivator::new(Activation::Starting);
    let proxy = spawn_proxy_with(routes, activator).await;

    let (status, body) = request_as_browser(proxy, "web.feat-1.myapp.localhost").await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.contains("<!doctype html>"), "returns HTML");
    assert!(
        body.contains("http-equiv=\"refresh\""),
        "without a self-reload the user has to hit refresh"
    );
    assert!(body.contains("web.feat-1.myapp.localhost"));
}

#[tokio::test]
async fn non_browser_clients_get_a_timeout_not_a_html_page() {
    // An agent's curl cannot make sense of an HTML waiting page.
    let routes = Routes::new();
    routes.insert(
        "web.feat-1.myapp.localhost",
        Route::stopped("myapp", "feat-1", "web"),
    );

    let activator = ScriptedActivator::new(Activation::Starting);
    let proxy = spawn_proxy_with(routes, activator).await;

    let (status, body) = request_through(proxy, "web.feat-1.myapp.localhost", "/").await;

    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    assert!(!body.contains("<!doctype"), "no HTML here: {body}");
    assert!(body.contains("minato status"), "gives something to go on");
}

#[tokio::test]
async fn failed_activation_explains_itself() {
    let routes = Routes::new();
    routes.insert(
        "web.feat-1.myapp.localhost",
        Route::stopped("myapp", "feat-1", "web"),
    );

    let activator = ScriptedActivator::new(Activation::Failed("no such image".into()));
    let proxy = spawn_proxy_with(routes, activator).await;

    let (status, body) = request_through(proxy, "web.feat-1.myapp.localhost", "/").await;

    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(
        body.contains("no such image"),
        "passes the reason through as-is"
    );
    assert!(body.contains("minato logs"));
}

#[tokio::test]
async fn activator_receives_normalised_hosts() {
    // Idle detection uses the same key as routing. Handing it a host with
    // a port would leave accesses unrecorded, and a service in active use
    // would be stopped as idle.
    let upstream = spawn_upstream().await;
    let routes = Routes::new();
    routes.insert(
        "web.feat-1.myapp.localhost",
        Route::new(upstream, "myapp", "feat-1", "web"),
    );

    let activator = ScriptedActivator::new(Activation::Ready(upstream));
    let proxy = spawn_proxy_with(routes, activator.clone()).await;

    // On a non-standard port the browser includes it in Host.
    request_through(proxy, "WEB.feat-1.myapp.localhost:8443", "/").await;

    let seen = activator.seen.lock().expect("lock").clone();
    assert_eq!(
        seen,
        vec!["web.feat-1.myapp.localhost".to_string()],
        "the port and the casing have to be dropped first"
    );
}

#[tokio::test]
async fn rejects_an_unusable_host_header() {
    let proxy = spawn_proxy(Routes::new()).await;
    let (status, _) = request_through(proxy, ":8080", "/").await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// An upstream that hangs up on its first `drops` connections and serves
/// normally after that — a dev server whose port is published before it
/// has bound it.
///
/// The counter is every connection it accepted, retries included.
async fn spawn_late_upstream(drops: usize) -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = accepted.clone();

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };

            if counter.fetch_add(1, Ordering::SeqCst) < drops {
                drop(stream);
                continue;
            }

            tokio::spawn(async move {
                let service = service_fn(|request: Request<Incoming>| async move {
                    Ok::<_, Infallible>(Response::new(text(&format!(
                        "upstream {}",
                        request.uri().path()
                    ))))
                });

                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    });

    (addr, accepted)
}

#[tokio::test]
async fn a_service_that_answers_late_is_tried_again() {
    // The cold-start 502: readiness said yes a moment before the app was
    // listening, and the request that woke it paid for the gap.
    let (upstream, accepted) = spawn_late_upstream(2).await;
    let routes = Routes::new();
    routes.insert(
        "web.feat-1.myapp.localhost",
        Route::stopped("myapp", "feat-1", "web"),
    );

    let activator = ScriptedActivator::new(Activation::Ready(upstream));
    let proxy = spawn_proxy_with(routes, activator).await;

    let (status, body) = request_through(proxy, "web.feat-1.myapp.localhost", "/woken").await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("upstream /woken"), "got: {body}");
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        3,
        "the two refusals and the one that answered"
    );
}

#[tokio::test]
async fn a_service_that_never_answers_still_reports_the_reason() {
    // Retrying must not swallow the explanation when it does not help.
    let (upstream, accepted) = spawn_late_upstream(usize::MAX).await;
    let routes = Routes::new();
    routes.insert(
        "web.feat-1.myapp.localhost",
        Route::stopped("myapp", "feat-1", "web"),
    );

    let activator = ScriptedActivator::new(Activation::Ready(upstream));
    let proxy = spawn_proxy_with(routes, activator).await;

    let (status, body) = request_through(proxy, "web.feat-1.myapp.localhost", "/").await;

    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(body.contains("minato status"), "got: {body}");
    assert!(
        accepted.load(Ordering::SeqCst) > 1,
        "it gave up without trying again"
    );
}

#[tokio::test]
async fn a_request_with_a_body_is_only_sent_once() {
    // A body cannot be read twice, and a POST that did get through would
    // be applied twice by a retry.
    let (upstream, accepted) = spawn_late_upstream(usize::MAX).await;
    let routes = Routes::new();
    routes.insert(
        "web.feat-1.myapp.localhost",
        Route::stopped("myapp", "feat-1", "web"),
    );

    let activator = ScriptedActivator::new(Activation::Ready(upstream));
    let proxy = spawn_proxy_with(routes, activator).await;

    let stream = TcpStream::connect(proxy).await.expect("connects");
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .expect("handshake");
    tokio::spawn(async move {
        let _ = connection.await;
    });

    let request = Request::builder()
        .method("POST")
        .uri("/orders")
        .header(HOST, "web.feat-1.myapp.localhost")
        .body(text("one order please"))
        .expect("builds");

    let response = sender.send_request(request).await.expect("gets a response");

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(accepted.load(Ordering::SeqCst), 1, "sent more than once");
}

#[tokio::test]
async fn a_running_service_is_not_retried() {
    // Retrying belongs to the cold start. A service that is up and not
    // answering is a real failure, and hiding it behind a delay helps
    // nobody.
    let (upstream, accepted) = spawn_late_upstream(usize::MAX).await;
    let routes = Routes::new();
    routes.insert(
        "web.feat-1.myapp.localhost",
        Route::new(upstream, "myapp", "feat-1", "web"),
    );

    let proxy = spawn_proxy(routes).await;
    let (status, _) = request_through(proxy, "web.feat-1.myapp.localhost", "/").await;

    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(accepted.load(Ordering::SeqCst), 1, "tried again anyway");
}
