//! What the screen does while the daemon works.
//!
//! `up` and `new` can take a minute. The event stream says what is
//! happening the whole way through (`docs/DESIGN.md` §3), so the question
//! is only how to show it.
//!
//! On a terminal: finished steps scroll up into the history and one line
//! stays put at the bottom, showing what is happening now. ratatui calls
//! this an inline viewport, and it is the reason there is a spinner at all
//! — a step can take thirty seconds and say nothing, and a display that
//! only moves when an event arrives is indistinguishable from a hang.
//!
//! Anywhere else — a pipe, a CI log, `TERM=dumb` — there is no line to
//! hold still. Steps are printed as they finish and that is all.

use std::io::{IsTerminal, Stdout, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use minato_api::{Event, LogLevel, StepStatus};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use ratatui::{Terminal, TerminalOptions, Viewport};

use super::surface::{Stream, Surface};
use super::theme;

/// One row: what is happening now. The history goes above it.
const VIEWPORT_HEIGHT: u16 = 1;

/// Fast enough to look alive, slow enough to be invisible on a battery.
const TICK: Duration = Duration::from_millis(120);

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// The live display. Cheap to clone: the Ctrl-C handler holds one too.
#[derive(Clone)]
pub struct Progress {
    state: Arc<Mutex<State>>,
    ticker: Arc<tokio::task::JoinHandle<()>>,
}

impl Progress {
    /// Takes over the bottom line, when there is one to take over.
    pub fn start() -> Self {
        let surface = Surface::stdout();

        let terminal = surface.is_interactive().then(open_viewport).flatten();

        let state = Arc::new(Mutex::new(State {
            terminal,
            running: Vec::new(),
            frame: 0,
        }));

        // Without this the spinner would only advance when the daemon had
        // something to say, which is exactly when it is least needed.
        let ticker = tokio::spawn({
            let state = Arc::clone(&state);
            async move {
                let mut interval = tokio::time::interval(TICK);
                loop {
                    interval.tick().await;
                    if let Ok(mut state) = state.lock() {
                        state.redraw();
                    }
                }
            }
        });

        Self {
            state,
            ticker: Arc::new(ticker),
        }
    }

    /// Folds one event from the daemon into the display.
    pub fn handle(&self, event: &Event) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };

        match event {
            Event::Step { id, label, status } => match status {
                StepStatus::Started => state.begin(id, label),
                StepStatus::Progress { message } => state.detail(id, message),
                StepStatus::Done => {
                    state.end(id);
                    state.emit(step_line("✓", theme::good(), label, None));
                }
                StepStatus::Failed { reason } => {
                    state.end(id);
                    state.emit_to(
                        Stream::Stderr,
                        step_line("✗", theme::bad(), label, Some(format!(": {reason}"))),
                    );
                }
                StepStatus::Skipped { reason } => {
                    state.end(id);
                    state.emit(step_line(
                        "-",
                        theme::muted(),
                        label,
                        Some(format!(" ({reason})")),
                    ));
                }
            },
            Event::Log { level, message } => match level {
                LogLevel::Debug => {}
                LogLevel::Info => state.emit(Line::from(vec![
                    Span::raw("  "),
                    Span::raw(message.clone()),
                ])),
                LogLevel::Warn => state.emit_to(
                    Stream::Stderr,
                    Line::from(vec![
                        Span::styled("  warning: ", theme::warn()),
                        Span::raw(message.clone()),
                    ]),
                ),
                LogLevel::Error => state.emit_to(
                    Stream::Stderr,
                    Line::from(vec![
                        Span::styled("  error: ", theme::bad()),
                        Span::raw(message.clone()),
                    ]),
                ),
            },
            // State changes show up in the summary, not on the way there.
            Event::ServiceState { .. } => {}
            Event::Output { line, .. } => state.emit(Line::from(vec![
                Span::styled("  │ ", theme::muted()),
                Span::raw(line.clone()),
            ])),
        }
    }

    /// Says something of the CLI's own, in the same place as the rest.
    pub fn say(&self, line: Line<'static>) {
        if let Ok(mut state) = self.state.lock() {
            state.emit_to(Stream::Stderr, line);
        }
    }

    /// Gives the bottom line back.
    ///
    /// Everything printed afterwards — the summary, an error — starts
    /// where the live line was, so nothing is left behind on screen.
    pub fn finish(&self) {
        self.ticker.abort();

        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let Some(mut terminal) = state.terminal.take() else {
            return;
        };

        let origin = terminal.get_frame().area();
        let _ = terminal.clear();
        let _ = terminal.set_cursor_position(Position::new(0, origin.y));
        let _ = terminal.show_cursor();
        let _ = std::io::stdout().flush();
    }
}

/// The backend a real run draws on. Generic only so that the tests can
/// put ratatui's own in its place and read back what was drawn.
type Screen = CrosstermBackend<Stdout>;

struct State<B: Backend = Screen> {
    /// `None` when nothing is watching, and after [`Progress::finish`].
    terminal: Option<Terminal<B>>,
    /// The steps in flight, oldest first.
    running: Vec<Step>,
    frame: usize,
}

struct Step {
    id: String,
    label: String,
    detail: Option<String>,
}

impl<B: Backend> State<B> {
    fn begin(&mut self, id: &str, label: &str) {
        self.running.push(Step {
            id: id.to_string(),
            label: label.to_string(),
            detail: None,
        });
        self.redraw();
    }

    fn detail(&mut self, id: &str, message: &str) {
        if let Some(step) = self.running.iter_mut().find(|step| step.id == id) {
            step.detail = Some(message.to_string());
        }
        self.redraw();
    }

    fn end(&mut self, id: &str) {
        self.running.retain(|step| step.id != id);
    }

    fn emit(&mut self, line: Line<'static>) {
        self.emit_to(Stream::Stdout, line);
    }

    /// Puts a line into the history above the live one.
    ///
    /// The stream only has a say when there is no viewport. A live display
    /// owns stdout — interleaving stderr into it by hand would tear the
    /// line it is holding still — so on a terminal everything goes there,
    /// and the separation is kept for the pipes that rely on it.
    fn emit_to(&mut self, stream: Stream, line: Line<'static>) {
        let Some(terminal) = &mut self.terminal else {
            Surface::for_stream(stream).print(|_| Loose(line.clone()));
            return;
        };

        let _ = terminal.insert_before(1, |buf: &mut Buffer| {
            line.render(buf.area, buf);
        });

        self.redraw();
    }

    fn redraw(&mut self) {
        let Some(terminal) = &mut self.terminal else {
            return;
        };

        self.frame = self.frame.wrapping_add(1);
        let spinner = SPINNER[(self.frame / 2) % SPINNER.len()];

        // The newest step is the one actually being waited on; the others
        // are counted rather than listed, because the line is one line.
        let line = match self.running.last() {
            Some(step) => {
                let mut spans = vec![
                    Span::styled(format!("  {spinner} "), theme::warn()),
                    Span::styled(step.label.clone(), theme::subject()),
                ];

                if let Some(detail) = &step.detail {
                    spans.push(Span::styled(format!(" · {detail}"), theme::muted()));
                }

                if self.running.len() > 1 {
                    spans.push(Span::styled(
                        format!("  (+{} more)", self.running.len() - 1),
                        theme::muted(),
                    ));
                }

                Line::from(spans)
            }
            None => Line::default(),
        };

        let _ = terminal.draw(|frame| {
            frame.render_widget(line, frame.area());
        });
    }
}

/// One line, drawn with nothing around it.
struct Loose(Line<'static>);

impl super::View for Loose {
    fn preferred_width(&self) -> u16 {
        u16::try_from(self.0.width()).unwrap_or(u16::MAX)
    }

    fn height(&self, _width: u16) -> u16 {
        1
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.0.clone().render(area, buf);
    }
}

fn step_line(
    symbol: &'static str,
    style: ratatui::style::Style,
    label: &str,
    suffix: Option<String>,
) -> Line<'static> {
    let mut spans = vec![
        Span::styled(format!("  {symbol} "), style),
        Span::raw(label.to_string()),
    ];

    if let Some(suffix) = suffix {
        spans.push(Span::styled(suffix, theme::muted()));
    }

    Line::from(spans)
}

/// Reserves the bottom line, or gives up quietly.
///
/// A terminal that will not report its size is not one to start drawing
/// on; the plain path prints the same steps and loses only the spinner.
fn open_viewport() -> Option<Terminal<Screen>> {
    if !std::io::stdout().is_terminal() {
        return None;
    }

    Terminal::with_options(
        CrosstermBackend::new(std::io::stdout()),
        TerminalOptions {
            viewport: Viewport::Inline(VIEWPORT_HEIGHT),
        },
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::render;

    use ratatui::backend::TestBackend;

    /// The state a pipe gets: no terminal, so nothing to hold still.
    fn plain() -> State {
        State {
            terminal: None,
            running: Vec::new(),
            frame: 0,
        }
    }

    /// A terminal in miniature, with the same inline viewport a real one
    /// gets. What it drew can be read straight back.
    fn watched(width: u16, height: u16) -> State<TestBackend> {
        let terminal = Terminal::with_options(
            TestBackend::new(width, height),
            TerminalOptions {
                viewport: Viewport::Inline(VIEWPORT_HEIGHT),
            },
        )
        .expect("opens");

        State {
            terminal: Some(terminal),
            running: Vec::new(),
            frame: 0,
        }
    }

    fn screen(state: &mut State<TestBackend>) -> String {
        let terminal = state.terminal.as_ref().expect("watched");
        let buffer = terminal.backend().buffer().clone();

        crate::ui::surface::render_to_string(&buffer, false)
    }

    #[test]
    fn the_live_line_names_what_is_happening_now() {
        let mut state = watched(48, 6);
        state.begin("pull", "pulling node:22-alpine");

        assert!(
            screen(&mut state).contains("pulling node:22-alpine"),
            "got:\n{}",
            screen(&mut state)
        );
    }

    #[test]
    fn progress_detail_joins_the_live_line() {
        let mut state = watched(60, 6);
        state.begin("build", "building web");
        state.detail("build", "step 3/8");

        let text = screen(&mut state);
        assert!(text.contains("building web"), "got:\n{text}");
        assert!(text.contains("step 3/8"), "got:\n{text}");
    }

    #[test]
    fn steps_beyond_the_one_line_are_counted_rather_than_dropped() {
        // The viewport is one row. Saying so beats silently showing one
        // of three services starting.
        let mut state = watched(60, 6);
        state.begin("a", "starting web");
        state.begin("b", "starting api");
        state.begin("c", "starting db");

        let text = screen(&mut state);
        assert!(text.contains("starting db"), "got:\n{text}");
        assert!(text.contains("(+2 more)"), "got:\n{text}");
    }

    #[test]
    fn a_finished_step_is_left_in_the_history_and_off_the_live_line() {
        // This is the whole shape of the display: what is done scrolls up
        // and stays, what is happening is redrawn in place.
        let mut state = watched(48, 6);
        state.begin("net", "creating the network");
        state.begin("pull", "pulling node:22-alpine");

        state.end("net");
        state.emit(step_line("✓", theme::good(), "creating the network", None));

        let text = screen(&mut state);
        assert!(text.contains("✓ creating the network"), "got:\n{text}");
        assert!(text.contains("pulling node:22-alpine"), "got:\n{text}");
        assert_eq!(
            text.matches("creating the network").count(),
            1,
            "the live line still names a finished step:\n{text}"
        );
    }

    #[test]
    fn the_live_line_empties_when_nothing_is_running() {
        let mut state = watched(48, 6);
        state.begin("only", "starting web");
        state.end("only");
        state.redraw();

        assert!(
            !screen(&mut state).contains("starting web"),
            "got:\n{}",
            screen(&mut state)
        );
    }

    #[test]
    fn a_step_is_forgotten_once_it_settles() {
        // Otherwise the live line would keep naming something that
        // finished a minute ago.
        let mut state = plain();
        state.begin("pull", "pulling node:22-alpine");
        state.begin("net", "creating the network");
        assert_eq!(state.running.len(), 2);

        state.end("pull");
        assert_eq!(state.running.len(), 1);
        assert_eq!(state.running[0].id, "net");
    }

    #[test]
    fn detail_attaches_to_the_step_it_names() {
        let mut state = plain();
        state.begin("build", "building web");
        state.begin("pull", "pulling postgres:17");
        state.detail("build", "step 3/8");

        assert_eq!(state.running[0].detail.as_deref(), Some("step 3/8"));
        assert_eq!(state.running[1].detail, None);
    }

    #[test]
    fn ending_a_step_that_never_started_is_not_a_panic() {
        // The daemon is free to report a step as done that it never
        // announced, and a crash here would take the whole command with it.
        let mut state = plain();
        state.end("never-started");
        assert!(state.running.is_empty());
    }

    #[test]
    fn a_finished_step_reads_the_same_as_it_used_to() {
        let text = render(&Loose(step_line(
            "✓",
            theme::good(),
            "creating the network",
            None,
        )));
        assert_eq!(text, "  ✓ creating the network\n");
    }

    #[test]
    fn a_failed_step_gives_its_reason() {
        let text = render(&Loose(step_line(
            "✗",
            theme::bad(),
            "starting web",
            Some(": port 3000 is in use".to_string()),
        )));

        assert_eq!(text, "  ✗ starting web: port 3000 is in use\n");
    }

    #[test]
    fn a_skipped_step_says_why() {
        let text = render(&Loose(step_line(
            "-",
            theme::muted(),
            "pulling node:22",
            Some(" (already present)".to_string()),
        )));

        assert_eq!(text, "  - pulling node:22 (already present)\n");
    }

    #[test]
    fn the_spinner_has_frames_to_cycle_through() {
        assert!(SPINNER.len() > 1);
        for frame in SPINNER {
            assert_eq!(frame.chars().count(), 1, "a wider frame would jitter");
        }
    }
}
