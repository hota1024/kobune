//! The full-screen dashboard: `kobune`, with nothing after it.
//!
//! This is the second half of what `docs/DESIGN.md` §3 set the views up
//! for. Nothing here draws a workspace — [`crate::ui::views::workspace`]
//! does, the same function `kobune status` prints with, handed a
//! [`ratatui::Frame`] instead of a buffer to fill. What is new is only
//! the parts a printed command has no use for: a cursor, a key, and a
//! clock.
//!
//! The clock is the reason it exists. Scale-to-zero stops services on its
//! own and another terminal can start them, so a listing printed once is
//! out of date within seconds — and running several worktrees at a time
//! is the thing Kobune is for.
//!
//! Three pieces, and each is testable without the other two:
//!
//! - [`app`] holds what is known and decides what a key means. No I/O
//! - [`draw`] turns that into a screen. A pure function
//! - [`daemon`] talks to the socket, over two channels
//!
//! What is left here is the loop between them, and the terminal.

mod ansi;
mod app;
mod daemon;
mod draw;
mod text;

use std::path::{Path, PathBuf};

use kobune_api::{Request, Response, Target};
use kobune_client::Client;
use ratatui::crossterm::event::{Event as TermEvent, KeyEvent};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::CliError;
use crate::ui::progress;

use app::{Action, App, Overlay};
use daemon::Update;

/// Opens the dashboard, and gives the terminal back when it closes.
pub async fn run(client: Client, cwd: PathBuf, workspace: Option<&str>) -> Result<(), CliError> {
    if !crate::attach::is_a_terminal() {
        return Err(CliError::Local(
            "the dashboard needs a terminal to draw on".to_string(),
        ));
    }

    let (mut connection, start) = client.connect_or_spawn().await?;

    // **Before the screen is taken.** No `kobune.toml`, not a git
    // repository, a daemon that will not come up: all of it fails here,
    // as the ordinary error with the ordinary hint, rather than as an
    // empty dashboard with a message written across it. It also means the
    // first frame is never blank.
    let workspaces = listing(&mut connection, &cwd).await?;

    let mut state = App::new(workspaces, &cwd, workspace);

    // The one thing a command would have printed on the way past. It
    // would be drawn over by the first frame, so it goes where the rest
    // of the trouble goes.
    if let Some(unprivileged) = crate::unprivileged_start(start, client.home()) {
        state.went_wrong(match &unprivileged.command {
            Some(command) => format!("{}. {} `{command}`", unprivileged.said, unprivileged.next),
            None => format!("{}. {}", unprivileged.said, unprivileged.next),
        });
    }

    let (commands, updates) = daemon::spawn(client, cwd, connection);
    let keys = watch_keys();

    let mut terminal = ratatui::try_init()
        .map_err(|err| CliError::Local(format!("cannot take the terminal: {err}")))?;

    let outcome = drive(&mut terminal, state, &commands, updates, keys).await;

    // The cursor is hidden by the first frame and `restore` does not put
    // it back, so a shell prompt with nothing to type at is what is left
    // without this. Same reason `attach::Screen::restore` shows it by
    // hand.
    let _ = terminal.show_cursor();
    ratatui::restore();

    outcome
}

/// Draw, wait for something to happen, draw again.
async fn drive(
    terminal: &mut ratatui::DefaultTerminal,
    mut state: App,
    commands: &tokio::sync::mpsc::UnboundedSender<daemon::Command>,
    mut updates: UnboundedReceiver<Update>,
    mut keys: UnboundedReceiver<KeyEvent>,
) -> Result<(), CliError> {
    loop {
        // The pane the log rows scroll inside, before anything is drawn
        // in it or any key is read about it.
        if let Ok(window) = terminal.size() {
            state.resize(draw::measure(&state, window.width, window.height));
        }

        terminal
            .draw(|frame| draw::draw(&state, frame))
            .map_err(|err| CliError::Local(format!("cannot draw: {err}")))?;

        if state.is_done() {
            return Ok(());
        }

        tokio::select! {
            key = keys.recv() => match key {
                Some(key) => match state.on_key(key) {
                    Some(Action::Ask(command)) => {
                        let _ = commands.send(command);
                    }
                    Some(Action::Open(url)) => open(&url, &mut state),
                    None => {}
                },
                // The keyboard is gone, which is not something to keep
                // drawing through.
                None => return Ok(()),
            },

            update = updates.recv() => match update {
                Some(update) => {
                    apply(&mut state, update);

                    // Everything else already in the queue, before
                    // drawing once for the lot. A dev server writing a
                    // hundred lines a second would otherwise repaint the
                    // screen a hundred times, and what a person can read
                    // is the last of them.
                    //
                    // Bounded, so a service writing faster than the
                    // screen can be drawn cannot hold the keyboard out
                    // of this loop.
                    for _ in 0..DRAIN_LIMIT {
                        let Ok(update) = updates.try_recv() else {
                            break;
                        };
                        apply(&mut state, update);
                    }
                }
                None => return Ok(()),
            },

            // Only while there is something to spin for. A step can take
            // thirty seconds and say nothing, and a display that moves
            // only when an event arrives is indistinguishable from a
            // hang — the reason `progress` has a spinner at all.
            () = tokio::time::sleep(progress::TICK), if state.activity().is_some() => {
                state.tick();
            }
        }
    }
}

/// How many queued updates one pass folds in before drawing again.
///
/// High enough that an ordinary burst of log lines costs one repaint,
/// low enough that a service writing without pause still gives the
/// keyboard a turn.
const DRAIN_LIMIT: usize = 512;

/// Folds one update into the state.
fn apply(state: &mut App, update: Update) {
    match update {
        Update::Listing(workspaces) => state.listing(workspaces),
        Update::Event(event) => state.on_event(&event),
        Update::Settled(outcome) => state.settled(outcome),
        Update::Trouble(message) => state.went_wrong(message),
        Update::Log { service, line } => state.on_log(service, line),
        Update::LogEnded(reason) => state.log_ended(reason),
        Update::Checks(report) => state.inspected(Overlay::Checks(report)),
        Update::Env { entries, service } => state.inspected(Overlay::Env { entries, service }),
        Update::InspectionFailed { what, reason } => {
            state.inspected(Overlay::Failed { what, reason });
        }
    }
}

/// The first listing, fetched before there is a screen to show it on.
async fn listing(
    connection: &mut kobune_client::Connection,
    cwd: &Path,
) -> Result<Vec<kobune_api::WorkspaceInfo>, CliError> {
    let request = Request::Ls {
        target: Target::new(cwd.to_path_buf()),
        all_projects: false,
    };

    match connection.request(request).await? {
        Response::Workspaces { workspaces } => Ok(workspaces),
        _ => Err(CliError::Local(
            "the daemon answered `ls` with something else".to_string(),
        )),
    }
}

/// Keys, off a thread of their own.
///
/// crossterm reads them by blocking, and the copy ratatui carries is
/// built without `event-stream`, so there is no future to await. A thread
/// and a channel is what `attach` does with the same problem.
///
/// Nothing joins it. It is blocked on the keyboard when the dashboard
/// closes, and the process is on its way out.
fn watch_keys() -> UnboundedReceiver<KeyEvent> {
    let (sender, receiver) = unbounded_channel();

    std::thread::Builder::new()
        .name("kobune-keys".to_string())
        .spawn(move || {
            loop {
                match ratatui::crossterm::event::read() {
                    // A resize needs no key: the loop redraws on
                    // anything, and the terminal reads its own size.
                    Ok(TermEvent::Resize(..)) => {
                        if sender.is_closed() {
                            return;
                        }
                    }
                    Ok(TermEvent::Key(key)) => {
                        if sender.send(key).is_err() {
                            return;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
        })
        .expect("spawns the thread");

    receiver
}

/// Hands a URL to the browser.
///
/// Not through the daemon: it has nothing to do with the environment, and
/// the GUI opens URLs the same way (`apps/desktop/src/app.rs`).
fn open(url: &str, state: &mut App) {
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(target_os = "linux")]
    let program = "xdg-open";
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let program = "start";

    // Its output would land on the screen this is drawing. The browser
    // says what it has to say in its own window.
    let spawned = std::process::Command::new(program)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    if let Err(err) = spawned {
        state.went_wrong(format!("cannot open {url}: {err}"));
    }
}
