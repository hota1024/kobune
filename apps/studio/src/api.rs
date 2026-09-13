//! HTTP, translated into the daemon's socket and back.
//!
//! Nothing here decides anything. Every handler opens a connection, asks
//! `kobune-api` the question the CLI would have asked, and serialises the
//! answer — which is what keeps the dashboard from growing a second,
//! slightly different idea of what a workspace is (`docs/DESIGN.md` §3).
//!
//! **One HTTP request is one connection.** §10 settled this for the TUI
//! and the reasoning carries: a `Connection` handles one request at a
//! time, so an `up` that takes a minute would otherwise stop the poll
//! that draws the screen for the whole minute it most needs to be drawn.
//! Here it falls out of the shape — a handler that returns drops its
//! connection.

use std::path::PathBuf;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, Sse};
use axum::response::{IntoResponse, Response};
use futures::stream::Stream;
use kobune_api::{Event, OutputStream, Request, Response as ApiResponse, Target};
use kobune_client::{Client, ClientError, Connection};
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::model::{
    ActionResult, DaemonView, DoctorView, EnvView, LogLine, StateView, TunnelView, WorkspaceView,
};

/// What every handler needs: where the project is, and how to reach the
/// daemon.
#[derive(Clone)]
pub struct Studio {
    pub client: Client,
    /// The directory the daemon resolves the project and `kobune.toml`
    /// from. The daemon does not know the caller's working directory, so
    /// every `Target` carries it.
    pub cwd: PathBuf,
}

impl Studio {
    /// The project, for what is not about one workspace — `doctor`, the
    /// tunnel, the listing, and creating a workspace that does not exist
    /// yet.
    fn project(&self) -> Target {
        Target::new(self.cwd.clone())
    }

    /// One workspace, named by **its own path** rather than by its label.
    ///
    /// **A label is not enough, because the main worktree has none.**
    /// `WorkspaceInfo::workspace` is `None` for it, and a `Target` with no
    /// label sends the daemon to `lookup_by_path(&repo.root, …)` — where
    /// `repo.root` is the worktree the target's `cwd` sits in
    /// (`kobune-core/src/git.rs`). Run `kobune studio` from inside a
    /// worktree and every action on the `(main)` row would land on *that*
    /// worktree instead, with `rm` deleting it while the page said it was
    /// removing main.
    ///
    /// Targeting by path is what the TUI does for the same reason
    /// (`apps/cli/src/ui/tui/daemon.rs`), and it needs no label: the
    /// worktree containing a workspace's own root is that workspace.
    fn workspace(&self, path: Option<String>) -> Target {
        match path {
            Some(path) => Target::new(PathBuf::from(path)),
            None => Target::new(self.cwd.clone()),
        }
    }

    async fn connect(&self) -> Result<Connection, Failed> {
        self.client.connect().await.map_err(Failed::from)
    }
}

/// An error on its way to the browser.
///
/// `hint` is carried through because `ClientError` has one and it is
/// usually the actionable half — "the daemon is not running" with
/// "run `kobune daemon start`" under it.
pub struct Failed {
    status: StatusCode,
    message: String,
    hint: Option<String>,
}

impl From<ClientError> for Failed {
    fn from(err: ClientError) -> Self {
        Self {
            // 502: this server is fine, the thing behind it is not. A 500
            // would send somebody reading logs to the wrong process.
            status: StatusCode::BAD_GATEWAY,
            hint: err.hint().map(str::to_string),
            message: err.to_string(),
        }
    }
}

impl Failed {
    fn unexpected(what: &str) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            message: format!("the daemon answered a {what} with something else"),
            hint: None,
        }
    }
}

impl IntoResponse for Failed {
    fn into_response(self) -> Response {
        let body = serde_json::json!({
            "error": self.message,
            "hint": self.hint,
        });
        (self.status, Json(body)).into_response()
    }
}

type Answer<T> = Result<Json<T>, Failed>;

/// The poll. Everything the chrome and the sidebar draw, in one round trip.
///
/// Three questions on one connection rather than three connections: they
/// are sequential anyway, and the screen is only true if they describe the
/// same instant.
pub async fn state(State(studio): State<Studio>) -> Answer<StateView> {
    let mut connection = studio.connect().await?;

    let pong = connection.handshake().await?;

    let workspaces = match connection
        .request(Request::Ls {
            target: studio.project(),
            all_projects: false,
        })
        .await?
    {
        ApiResponse::Workspaces { workspaces } => workspaces,
        _ => return Err(Failed::unexpected("listing")),
    };

    let project = workspaces.first().map(|w| w.project.clone());

    // A tunnel that was never set up is not an error, and a daemon that
    // cannot answer about one should not blank the whole screen — the
    // workspaces are already in hand and they are what the page is for.
    let tunnel = match connection
        .request(Request::TunnelStatus {
            target: studio.project(),
        })
        .await
    {
        Ok(ApiResponse::Tunnel(info)) => TunnelView::from(info),
        _ => TunnelView::unknown(),
    };

    Ok(Json(StateView {
        daemon: DaemonView::from(pong),
        project,
        tunnel,
        workspaces: workspaces.into_iter().map(WorkspaceView::from).collect(),
    }))
}

/// `kobune doctor`, behind the top bar's button.
pub async fn doctor(State(studio): State<Studio>) -> Answer<DoctorView> {
    let mut connection = studio.connect().await?;

    match connection
        .request(Request::Doctor {
            target: studio.project(),
        })
        .await?
    {
        ApiResponse::Diagnostics(diagnostics) => Ok(Json(DoctorView { diagnostics })),
        _ => Err(Failed::unexpected("doctor")),
    }
}

#[derive(Debug, Deserialize)]
pub struct EnvQuery {
    /// The workspace's own path. See [`Studio::workspace`].
    path: Option<String>,
    service: Option<String>,
}

/// `kobune env ls`, masked.
///
/// **`reveal` is not wired to anything.** The daemon will hand over
/// resolved secrets for the asking, and a browser tab is a place where a
/// value stays on screen behind whoever walks past — `docs/DESIGN.md` §8
/// masks by default for that reason, and a dashboard is the weakest
/// possible place to make an exception.
pub async fn env(State(studio): State<Studio>, Query(query): Query<EnvQuery>) -> Answer<EnvView> {
    let mut connection = studio.connect().await?;

    match connection
        .request(Request::EnvList {
            target: studio.workspace(query.path),
            reveal: false,
            service: query.service,
        })
        .await?
    {
        ApiResponse::Env { entries, service } => Ok(Json(EnvView { service, entries })),
        _ => Err(Failed::unexpected("env listing")),
    }
}

/// Something to do to a workspace or a service.
///
/// `path` is the workspace's own root, not a label — see
/// [`Studio::workspace`] for why a label cannot name the main worktree.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    Up {
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        services: Vec<String>,
        #[serde(default)]
        rebuild: bool,
    },
    Down {
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        services: Vec<String>,
    },
    /// Down, then up. The palette's "Restart web".
    Restart {
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        services: Vec<String>,
    },
    Rm {
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        force: bool,
    },
    New {
        branch: String,
        #[serde(default)]
        base: Option<String>,
        #[serde(default = "yes")]
        start: bool,
    },
}

fn yes() -> bool {
    true
}

/// Runs one action and reports what happened.
///
/// The events an action produces — the steps `kobune up` prints — are
/// dropped rather than streamed. The screen is a poll, and a start that
/// takes a minute shows up on it as services moving through `starting`;
/// a second channel for the same news would be a second thing to keep
/// true. `reason` on a failed service is where the detail already lives.
pub async fn act(State(studio): State<Studio>, Json(action): Json<Action>) -> Answer<ActionResult> {
    let mut connection = studio.connect().await?;

    let requests = match action {
        Action::Up {
            path,
            services,
            rebuild,
        } => vec![Request::Up {
            target: studio.workspace(path),
            services,
            rebuild,
        }],
        Action::Down { path, services } => vec![Request::Down {
            target: studio.workspace(path),
            services,
            all: false,
        }],
        Action::Restart { path, services } => vec![
            Request::Down {
                target: studio.workspace(path.clone()),
                services: services.clone(),
                all: false,
            },
            Request::Up {
                target: studio.workspace(path),
                services,
                rebuild: false,
            },
        ],
        Action::Rm { path, force } => vec![Request::Rm {
            target: studio.workspace(path),
            force,
        }],
        Action::New {
            branch,
            base,
            start,
        } => vec![Request::New {
            target: studio.project(),
            branch,
            base,
            path: None,
            start,
            rebuild: false,
        }],
    };

    for request in requests {
        connection.call(request, |_| {}).await?;
    }

    Ok(Json(ActionResult {
        ok: true,
        message: None,
    }))
}

#[derive(Debug, Deserialize)]
pub struct LogQuery {
    /// The workspace's own path. See [`Studio::workspace`].
    path: Option<String>,
    /// One service, or every service in the workspace when absent — which
    /// is what the pane's `all` filter is.
    service: Option<String>,
    #[serde(default)]
    follow: bool,
    tail: Option<usize>,
}

/// The log pane, as server-sent events.
///
/// **Closing the pane cancels the follow.** §10 calls this out for the
/// TUI and it is sharper in a browser, where a tab left open is the normal
/// case: the stream's guard is dropped when the response body is, the
/// daemon is asked to stop, and it lets go of the runtime's log stream at
/// once. Without it every opened pane would leak a follower for as long as
/// the daemon lived.
pub async fn logs(
    State(studio): State<Studio>,
    Query(query): Query<LogQuery>,
) -> Result<Sse<impl Stream<Item = Result<SseEvent, std::convert::Infallible>>>, Failed> {
    let mut connection = studio.connect().await?;

    let services = query.service.into_iter().collect::<Vec<_>>();
    let request = Request::Logs {
        target: studio.workspace(query.path),
        services,
        follow: query.follow,
        tail: query.tail.or(Some(500)),
        attach: None,
    };

    let (lines, receiver) = mpsc::unbounded_channel::<LogLine>();
    // Resolves when the browser goes away: `alive` is moved into the
    // stream below, so it is dropped exactly when the response body is.
    let (alive, closed) = oneshot::channel::<()>();

    tokio::spawn(async move {
        let outcome = connection
            .call_until(
                request,
                |event| {
                    let line = match event {
                        Event::Output {
                            service,
                            stream,
                            line,
                        } => LogLine {
                            service,
                            stream: match stream {
                                OutputStream::Stdout => "stdout".into(),
                                OutputStream::Stderr => "stderr".into(),
                            },
                            text: line,
                        },
                        // The daemon's own commentary — "waiting for api",
                        // and why a follow stopped. The pane shows it
                        // rather than swallowing it, because when a follow
                        // ends this is the only thing that says why.
                        Event::Log { message, .. } => LogLine {
                            service: None,
                            stream: "log".into(),
                            text: message,
                        },
                        _ => return,
                    };

                    // The receiver is unbounded and a dead one only means
                    // the browser left, which `closed` is already handling.
                    let _ = lines.send(line);
                },
                async {
                    let _ = closed.await;
                },
            )
            .await;

        if let Err(err) = outcome {
            tracing::debug!("the log stream ended: {err}");
        }
    });

    let stream = UnboundedReceiverStream::new(receiver).map(move |line| {
        // Keeps the guard alive for as long as the browser is reading.
        let _alive = &alive;
        Ok(SseEvent::default()
            .json_data(line)
            .unwrap_or_else(|_| SseEvent::default().data("{\"stream\":\"log\",\"text\":\"\"}")))
    });

    Ok(Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default()))
}
