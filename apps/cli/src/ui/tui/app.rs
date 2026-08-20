//! What the dashboard knows, and what a key does to it.
//!
//! No I/O of any kind. A key goes in, the state changes, and what has to
//! happen elsewhere comes back as an [`Action`] for the loop to carry
//! out. That is what lets every rule in here be asserted on without a
//! terminal, a daemon or a clock.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use kobune_api::{Event, LogLevel, ServiceInfo, StepStatus, WorkspaceInfo};
use kobune_core::ServiceState;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr as _;

use crate::ui::theme;

use super::ansi;
use super::daemon::Command;
use super::text::{fit, pad};

/// How many log lines are kept.
///
/// Enough to scroll back over what a service said while it was starting,
/// and bounded so that a service in a restart loop cannot fill memory
/// with the same line. The GUI's log pane keeps the same number.
const MAX_LOG_LINES: usize = 2000;

/// The widest the service column gets. Past this it is eating the line it
/// is there to label.
const MAX_SERVICE_COLUMN: usize = 14;

/// The space between that column and the text.
const SERVICE_GAP: u16 = 2;

/// The layout, as the drawing last worked it out.
///
/// Handed to the state rather than guessed at by it: how far a thing
/// scrolls depends on how much of it there is and how much of it shows,
/// and only the drawing knows either.
#[derive(Debug, Clone, Copy)]
pub struct Measured {
    pub log_columns: u16,
    pub log_rows: u16,
    pub overlay_rows: u16,
    pub overlay_content: u16,
}

/// Which pane the keys are talking to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Workspaces,
    Services,
    Logs,
}

/// Something drawn over the dashboard until it is dismissed.
///
/// **Every one of these is a view the printed commands already had.**
/// `kobune doctor`, `kobune env ls` and `kobune url --qr` build panels
/// out of the daemon's answers, and `Framed` puts a panel in a frame — so
/// what each of these costs is a request, a key and somewhere to sit.
/// That was the point of `docs/DESIGN.md` §3's split.
pub enum Overlay {
    /// The key list, which asks the daemon nothing.
    Keys,
    /// A request is out. Named, so the box says what it is waiting for.
    Waiting(&'static str),
    /// `kobune doctor`.
    Checks(Box<kobune_api::Diagnostics>),
    /// `kobune env ls`, masked as the printed one masks.
    Env {
        entries: Vec<kobune_api::EnvInfo>,
        service: Option<String>,
    },
    /// A service's URL, as a code to photograph.
    Code(Box<ServiceInfo>),
    /// The request came back with nothing to show.
    Failed { what: &'static str, reason: String },
}

impl Overlay {
    /// What this is *about*, which is not the same as what it is showing.
    ///
    /// Waiting for the checks, the checks themselves, and the reason
    /// they could not be read are three states of one thing to the
    /// person who pressed `c`. The key that opened it closes it in any
    /// of them, and an answer only lands in the box that was waiting for
    /// it — otherwise a reply arriving after somebody pressed esc would
    /// reopen the overlay they had just dismissed.
    fn kind(&self) -> &'static str {
        match self {
            Self::Keys => "keys",
            Self::Code(_) => "code",
            Self::Checks(_) => "the checks",
            Self::Env { .. } => "the environment",
            Self::Waiting(what) | Self::Failed { what, .. } => what,
        }
    }

    fn is_like(&self, other: &Self) -> bool {
        self.kind() == other.kind()
    }
}

/// What a log pane is following.
///
/// **Held rather than read off the cursor each time.** Moving the cursor
/// to look at another workspace would otherwise tear down the stream and
/// open another, which costs a connection and a runtime log stream per
/// keypress and loses everything scrolled back to. A second `l` is how
/// the subject changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subject {
    pub path: PathBuf,
    /// The workspace's name, for the heading.
    pub workspace: String,
    /// One service, or every service in the workspace.
    pub service: Option<String>,
}

/// One line a service wrote.
#[derive(Debug, Clone)]
pub struct LogLine {
    /// Which service wrote it, where the daemon said.
    pub service: Option<String>,
    pub line: String,
}

/// The log pane's contents.
pub struct Logs {
    pub subject: Subject,
    lines: VecDeque<LogLine>,
    /// Rows between the newest row and the bottom of the view. Zero is
    /// following.
    ///
    /// **Rows, not lines.** A line too wide for the pane is several rows,
    /// and a page-up that moved by lines would jump a screenful further
    /// than the screen it was named after.
    scrolled: usize,
    viewport: Viewport,
    /// What the pane says in place of `following`, once the stream is
    /// over.
    pub ended: Option<String>,
}

/// The pane as it was last drawn.
///
/// Scrolling is measured in the rows a reader can actually see, and a row
/// is only a row once there is a width to wrap to — so the size the pane
/// was given comes back here rather than being guessed at.
#[derive(Debug, Clone, Copy)]
struct Viewport {
    columns: u16,
    rows: u16,
}

impl Default for Viewport {
    /// What a pane that has not been drawn yet is assumed to be. Only the
    /// keys pressed before the first frame ever see it.
    fn default() -> Self {
        Self {
            columns: 80,
            rows: 10,
        }
    }
}

impl Logs {
    fn new(subject: Subject) -> Self {
        Self {
            subject,
            lines: VecDeque::new(),
            scrolled: 0,
            viewport: Viewport::default(),
            ended: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Whether the view is pinned to the newest row.
    pub fn following(&self) -> bool {
        self.scrolled == 0
    }

    /// How many rows there are below the view — what has arrived while
    /// somebody was reading further up.
    pub fn behind(&self) -> usize {
        self.scrolled
    }

    /// The rows to draw, top to bottom.
    ///
    /// **Wrapped, not cut.** A line wider than the pane used to lose its
    /// tail without saying so, and the lines that overflow are the ones
    /// worth reading: a stack trace, a failing assertion, a URL. A
    /// terminal wraps what `kobune logs` prints, and this is the same
    /// text.
    pub fn rows(&self) -> Vec<Line<'static>> {
        let height = usize::from(self.viewport.rows);
        if height == 0 {
            return Vec::new();
        }

        let column = self.column_width();
        let width = self.wrap_width();

        // Backwards from the newest line, and only far enough to fill
        // what is scrolled past plus the pane itself. Wrapping every line
        // in the buffer to find out would be two thousand of them for a
        // pane ten rows tall.
        let wanted = self.scrolled.saturating_add(height);
        let mut collected: Vec<Line<'static>> = Vec::new();

        for line in self.lines.iter().rev() {
            let mut rows = rows_of(line, width, column);
            rows.reverse();
            collected.append(&mut rows);

            if collected.len() >= wanted {
                break;
            }
        }

        // Scrolled further than there is to scroll — lines fell off the
        // front while somebody was reading — the view sits at the top
        // rather than going blank.
        let skip = self.scrolled.min(collected.len().saturating_sub(height));

        let mut window: Vec<Line<'static>> =
            collected.into_iter().skip(skip).take(height).collect();
        window.reverse();
        window
    }

    /// The pane's size, as it was last drawn.
    ///
    /// The offset is re-clamped against it: a window that grew, or one
    /// that widened so that lines re-wrapped into fewer rows, leaves the
    /// view scrolled past a bottom that has moved up to meet it.
    fn resize(&mut self, columns: u16, rows: u16) {
        self.viewport = Viewport { columns, rows };
        self.scrolled = self.scrolled.min(self.scrolled_max());
    }

    /// How wide the service column is, or zero when there is none.
    ///
    /// Measured over the whole buffer rather than over what is on screen:
    /// a column that changed width as the view scrolled would make every
    /// line appear to shift sideways.
    fn column_width(&self) -> usize {
        // Following one service, its name written down the side of its
        // own output says nothing.
        if self.subject.service.is_some() {
            return 0;
        }

        self.lines
            .iter()
            .filter_map(|line| line.service.as_deref())
            .map(str::width)
            .max()
            .unwrap_or(0)
            .min(MAX_SERVICE_COLUMN)
    }

    /// The room a line's own text has, once the column has had its share.
    fn wrap_width(&self) -> u16 {
        let column = self.column_width();
        let taken = u16::try_from(column)
            .unwrap_or(0)
            .saturating_add(if column > 0 { SERVICE_GAP } else { 0 });

        // Never zero: a width of nothing has no rows to wrap into, and
        // the caller would loop over a buffer that never fills.
        self.viewport.columns.saturating_sub(taken).max(1)
    }

    /// How far back the view may go: far enough to put the oldest row at
    /// the top of the pane, and no further.
    fn scrolled_max(&self) -> usize {
        let column = self.column_width();
        let width = self.wrap_width();

        let total: usize = self
            .lines
            .iter()
            .map(|line| rows_of(line, width, column).len())
            .sum();

        total.saturating_sub(usize::from(self.viewport.rows))
    }

    fn push(&mut self, line: LogLine) {
        // Only matters while somebody is reading further up. Following,
        // the view is the tail whatever arrives, and wrapping every line
        // to find out how tall it is would be work for nothing.
        let holding = self.scrolled > 0;
        let width = self.wrap_width();
        let column = self.column_width();

        let mut dropped = 0;
        if self.lines.len() >= MAX_LOG_LINES
            && let Some(oldest) = self.lines.pop_front()
            && holding
        {
            dropped = rows_of(&oldest, width, column).len();
        }

        let added = if holding {
            rows_of(&line, width, column).len()
        } else {
            0
        };

        self.lines.push_back(line);

        // Scrolled up, the view stays on the rows it was showing rather
        // than sliding as new ones arrive underneath.
        if holding {
            self.scrolled = self.scrolled.saturating_add(added).saturating_sub(dropped);
        }
    }

    /// Moves the view back through the rows by `rows`, or forward by a
    /// negative one.
    fn scroll(&mut self, rows: isize) {
        self.scrolled = self
            .scrolled
            .saturating_add_signed(rows)
            .min(self.scrolled_max());
    }
}

/// One line as the rows it occupies.
///
/// The service column is on the first of them and the rest are indented
/// under it, so a wrapped line reads as one line rather than as one line
/// and an unlabelled one.
fn rows_of(entry: &LogLine, width: u16, column: usize) -> Vec<Line<'static>> {
    let wrapped = crate::ui::panel::wrap(&[ansi::line(&entry.line)], width);

    if column == 0 {
        return wrapped;
    }

    wrapped
        .into_iter()
        .enumerate()
        .map(|(index, row)| {
            let label = match index {
                0 => entry.service.as_deref().unwrap_or(""),
                _ => "",
            };

            let mut spans = vec![
                Span::styled(pad(&fit(label, column), column), theme::secondary()),
                Span::raw(" ".repeat(usize::from(SERVICE_GAP))),
            ];
            spans.extend(row.spans);

            Line::from(spans)
        })
        .collect()
}

/// What a key asks for that this module cannot do itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Something for the daemon.
    Ask(Command),
    /// A URL for the browser. Nothing to do with the daemon, so it does
    /// not go through it.
    Open(String),
}

/// An operation in flight, and how far the daemon says it has got.
#[derive(Debug)]
pub struct Activity {
    /// Which workspace it is happening to.
    ///
    /// The events carry a service's name and not a workspace's, because
    /// a request is already about one. This is that one.
    path: PathBuf,
    /// What was asked for. It is what the line says until the daemon has
    /// named a step of its own — an `up` that spends four seconds
    /// resolving a config would otherwise show a spinner against nothing.
    pub label: String,
    /// The steps in flight, oldest last. The same shape
    /// [`crate::ui::Progress`] keeps, and for the same reason: the newest
    /// is the one actually being waited on.
    pub steps: Vec<Step>,
}

#[derive(Debug)]
pub struct Step {
    id: String,
    pub label: String,
    pub detail: Option<String>,
}

/// The dashboard.
pub struct App {
    workspaces: Vec<WorkspaceInfo>,
    focus: Focus,
    /// **By path, not by position.** The listing is re-fetched every few
    /// seconds and comes back reordered, longer or shorter — a worktree
    /// added in another terminal, one that `rm` took away. Holding an
    /// index means the cursor lands on a different workspace the instant
    /// a refresh arrives, and the next `d` stops the wrong one. A path is
    /// what identifies a worktree, and it is what the requests are built
    /// from too.
    selected: Option<PathBuf>,
    /// The same idea one level down, by name: services are per workspace,
    /// so a name is unique where it is used.
    selected_service: Option<String>,
    activity: Option<Activity>,
    /// Something that went wrong and has not been read yet.
    trouble: Option<String>,
    logs: Option<Logs>,
    /// Whether the log pane has the screen to itself.
    logs_full: bool,
    overlay: Option<Overlay>,
    /// How far the overlay has been scrolled, in rows.
    overlay_scroll: usize,
    /// How tall an overlay may be drawn, and how tall the one on screen
    /// is, as they were last laid out.
    overlay_rows: u16,
    overlay_content: u16,
    /// Advanced by the loop while there is activity, and read by the
    /// spinner.
    frame: usize,
    done: bool,
}

impl App {
    /// Opens on a listing that has already been fetched.
    ///
    /// There is always one: the first request is made before the screen
    /// is taken, so a failure is an ordinary error rather than an empty
    /// dashboard with a message in it.
    ///
    /// `at` is where it was run, and `named` is a `--workspace` if one
    /// was given. Between them they decide where the cursor starts —
    /// [`opening_on`] says how.
    pub fn new(workspaces: Vec<WorkspaceInfo>, at: &Path, named: Option<&str>) -> Self {
        let selected = opening_on(&workspaces, at, named).map(|workspace| workspace.path.clone());

        Self {
            workspaces,
            focus: Focus::Workspaces,
            selected,
            selected_service: None,
            activity: None,
            trouble: None,
            logs: None,
            logs_full: false,
            overlay: None,
            overlay_scroll: 0,
            overlay_rows: 20,
            overlay_content: 0,
            frame: 0,
            done: false,
        }
    }

    pub fn workspaces(&self) -> &[WorkspaceInfo] {
        &self.workspaces
    }

    pub fn focus(&self) -> Focus {
        self.focus
    }

    pub fn activity(&self) -> Option<&Activity> {
        self.activity.as_ref()
    }

    pub fn trouble(&self) -> Option<&str> {
        self.trouble.as_deref()
    }

    pub fn overlay(&self) -> Option<&Overlay> {
        self.overlay.as_ref()
    }

    pub fn overlay_scroll(&self) -> u16 {
        u16::try_from(self.overlay_scroll).unwrap_or(u16::MAX)
    }

    pub fn logs(&self) -> Option<&Logs> {
        self.logs.as_ref()
    }

    /// Whether the log pane has taken the whole screen.
    ///
    /// False whenever there is no pane, so a caller never has to ask two
    /// questions to find out what to draw.
    pub fn logs_are_full(&self) -> bool {
        self.logs_full && self.logs.is_some()
    }

    pub fn frame(&self) -> usize {
        self.frame
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    /// The project every workspace here belongs to.
    ///
    /// One request, one project (`all_projects` is not asked for), so the
    /// first is the answer. Empty when there is nothing at all, which the
    /// title then leaves off rather than showing a stray separator.
    pub fn project(&self) -> &str {
        self.workspaces
            .first()
            .map(|workspace| workspace.project.as_str())
            .unwrap_or_default()
    }

    pub fn selected_index(&self) -> Option<usize> {
        let selected = self.selected.as_deref()?;
        self.workspaces
            .iter()
            .position(|workspace| workspace.path == selected)
    }

    pub fn selected(&self) -> Option<&WorkspaceInfo> {
        self.workspaces.get(self.selected_index()?)
    }

    pub fn selected_service_index(&self) -> Option<usize> {
        let name = self.selected_service.as_deref()?;
        self.selected()?
            .services
            .iter()
            .position(|service| service.name == name)
    }

    pub fn selected_service(&self) -> Option<&ServiceInfo> {
        let workspace = self.selected()?;
        workspace.services.get(self.selected_service_index()?)
    }

    /// Folds a fresh listing in, keeping the cursor where the person put
    /// it.
    ///
    /// When the selected workspace is gone, the position it held is the
    /// next best answer — the neighbour, rather than jumping back to the
    /// top of a list somebody was halfway down.
    pub fn listing(&mut self, workspaces: Vec<WorkspaceInfo>) {
        let previous = self.selected_index();

        self.workspaces = workspaces;

        if self.selected_index().is_none() {
            let fallback = previous
                .unwrap_or(0)
                .min(self.workspaces.len().saturating_sub(1));

            self.selected = self
                .workspaces
                .get(fallback)
                .map(|workspace| workspace.path.clone());
        }

        // The service the cursor was on may have gone with a
        // configuration change. Focus follows it back out rather than
        // pointing at nothing.
        if self.selected_service.is_some() && self.selected_service_index().is_none() {
            self.selected_service = None;
            self.focus = Focus::Workspaces;
        }
    }

    /// Folds one event from an operation into what is on screen.
    ///
    /// Most of it lands on the line at the bottom, which holds one thing
    /// at a time. The exception is a service changing state: that goes
    /// straight into the listing, so the pane on the right shows `ready`
    /// the moment the daemon says so rather than up to three seconds
    /// later when the next listing lands.
    pub fn on_event(&mut self, event: &Event) {
        if let Event::ServiceState {
            service,
            state,
            reason,
        } = event
        {
            self.service_changed(service, state, reason.as_deref());
            return;
        }

        let Some(activity) = &mut self.activity else {
            return;
        };

        match event {
            Event::Step { id, label, status } => match status {
                StepStatus::Started => activity.steps.push(Step {
                    id: id.clone(),
                    label: label.clone(),
                    detail: None,
                }),
                StepStatus::Progress { message } => {
                    if let Some(step) = activity.steps.iter_mut().find(|step| &step.id == id) {
                        step.detail = Some(message.clone());
                    }
                }
                StepStatus::Done | StepStatus::Skipped { .. } => {
                    activity.steps.retain(|step| &step.id != id);
                }
                StepStatus::Failed { reason } => {
                    activity.steps.retain(|step| &step.id != id);
                    self.trouble = Some(format!("{label}: {reason}"));
                }
            },
            // A warning survives the operation it came from; the rest is
            // detail that the state on screen will show in a moment
            // anyway.
            Event::Log { level, message } => {
                if matches!(level, LogLevel::Warn | LogLevel::Error) {
                    self.trouble = Some(message.clone());
                }
            }
            // Handled above, before the line at the bottom gets a look.
            Event::ServiceState { .. }
            | Event::Output { .. }
            | Event::Attached { .. }
            | Event::Bytes { .. } => {}
        }
    }

    /// A service the operation in flight is acting on changed state.
    ///
    /// **Only the workspace the request was about.** The event names a
    /// service and not a workspace, because the request already settled
    /// that; a shared `scope = "project"` service belongs to several, and
    /// writing this into all of them would be inventing a fact about the
    /// ones nobody asked about. The next listing squares them up.
    fn service_changed(&mut self, name: &str, state: &ServiceState, reason: Option<&str>) {
        let Some(activity) = &self.activity else {
            return;
        };

        let Some(workspace) = self
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.path == activity.path)
        else {
            return;
        };

        let Some(service) = workspace
            .services
            .iter_mut()
            .find(|service| service.name == name)
        else {
            return;
        };

        service.state = state.clone();
        service.reason = reason.map(str::to_string);
    }

    /// The operation finished, one way or the other.
    pub fn settled(&mut self, outcome: Result<(), String>) {
        self.activity = None;

        if let Err(reason) = outcome {
            self.trouble = Some(reason);
        }
    }

    /// Something went wrong that no operation asked for — the listing
    /// could not be re-fetched, the daemon went away.
    pub fn went_wrong(&mut self, message: String) {
        self.trouble = Some(message);
    }

    pub fn tick(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    /// The pane the log rows are scrolled inside, as it was last drawn.
    ///
    /// Told rather than assumed: a row is only a row once there is a
    /// width to wrap to, and a page is only a page once there is a
    /// height.
    pub fn resize(&mut self, measured: Measured) {
        if let Some(logs) = &mut self.logs {
            logs.resize(measured.log_columns, measured.log_rows);
        }

        self.overlay_rows = measured.overlay_rows;
        self.overlay_content = measured.overlay_content;

        // A window that grew leaves a scroll offset pointing past what
        // there now is to scroll: the log pane said `paused · 15 below`
        // with the whole buffer on screen and nothing below it.
        self.overlay_scroll = self.overlay_scroll.min(self.overlay_scrolled_max());
    }

    /// The one way in for a key.
    pub fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        // Windows reports the release as well as the press, and acting on
        // both runs everything twice.
        if key.kind != KeyEventKind::Press {
            return None;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
        {
            self.done = true;
            return None;
        }

        // An overlay is read, and often scrolled, so the keys that move
        // through it belong to it while it is up. Everything else still
        // does what it does — the one thing worse than an overlay you
        // cannot scroll is one you cannot get out from behind.
        if self.overlay.is_some() {
            match key.code {
                KeyCode::Esc => {
                    self.overlay = None;
                    return None;
                }
                KeyCode::Up | KeyCode::Char('k') => return self.scroll_overlay(-1),
                KeyCode::Down | KeyCode::Char('j') => return self.scroll_overlay(1),
                KeyCode::PageUp => return self.scroll_overlay(-self.overlay_page()),
                KeyCode::PageDown => return self.scroll_overlay(self.overlay_page()),
                KeyCode::Home | KeyCode::Char('g') => return self.scroll_overlay(isize::MIN),
                KeyCode::End | KeyCode::Char('G') => return self.scroll_overlay(isize::MAX),

                // The keys that open one. They fall through, and the arm
                // below either swaps this overlay for that one or closes
                // it because it is already the one being asked for.
                KeyCode::Char('?' | 'c' | 'e' | 'Q') => {}

                // `q` leaves from anywhere.
                KeyCode::Char('q') => {}

                // Anything else puts it away and then does its job.
                _ => self.overlay = None,
            }
        }

        match key.code {
            KeyCode::Char('q') => self.done = true,

            // Esc backs out of one layer at a time, innermost first, and
            // only leaves when there is nothing left to back out of.
            // `q` is the one that always leaves.
            KeyCode::Esc => {
                if self.logs_are_full() {
                    self.logs_full = false;
                } else if self.logs.is_some() {
                    return Some(self.close_logs());
                } else {
                    self.done = true;
                }
            }

            KeyCode::Char('?') => self.show(Overlay::Keys),
            KeyCode::Char('c') => return self.inspect(Overlay::Waiting("the checks")),
            KeyCode::Char('e') => return self.inspect(Overlay::Waiting("the environment")),
            KeyCode::Char('Q') => return self.show_code(),

            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::PageUp => self.page(-1),
            KeyCode::PageDown => self.page(1),

            // The oldest line kept, and the newest. In the other panes
            // they are the first and last row, which is the same idea.
            KeyCode::Home | KeyCode::Char('g') => self.move_by(isize::MIN),
            KeyCode::End | KeyCode::Char('G') => self.move_by(isize::MAX),

            KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => self.switch_pane(),

            KeyCode::Char('r') => return Some(Action::Ask(Command::Refresh)),
            KeyCode::Char('u') => return self.operate(true),
            KeyCode::Char('d') => return self.operate(false),
            KeyCode::Char('o') => return self.open(),
            KeyCode::Char('l') => return self.toggle_logs(),
            KeyCode::Char('L') => return self.toggle_full(),

            _ => return None,
        }

        None
    }

    /// Puts an overlay up, or takes down the one already there when it
    /// is the same kind.
    fn show(&mut self, overlay: Overlay) {
        if self
            .overlay
            .as_ref()
            .is_some_and(|open| open.is_like(&overlay))
        {
            self.overlay = None;
            return;
        }

        self.overlay = Some(overlay);
        self.overlay_scroll = 0;
    }

    /// One of the two overlays that has to be fetched first.
    fn inspect(&mut self, waiting: Overlay) -> Option<Action> {
        let Overlay::Waiting(what) = waiting else {
            return None;
        };

        if self
            .overlay
            .as_ref()
            .is_some_and(|open| open.is_like(&Overlay::Waiting(what)))
        {
            self.overlay = None;
            return None;
        }

        let workspace = self.selected()?;
        let path = workspace.path.clone();
        let service = self.selected_service().map(|service| service.name.clone());

        // Up before the answer arrives, so a slow daemon looks like a
        // slow daemon rather than like a key that did nothing.
        self.overlay = Some(Overlay::Waiting(what));
        self.overlay_scroll = 0;

        Some(Action::Ask(match what {
            "the checks" => Command::Checks { path },
            _ => Command::Env { path, service },
        }))
    }

    /// `Q`: the URL as a code to photograph.
    ///
    /// Needs nothing from the daemon — the listing already carries the
    /// URL, and `views::url` is what draws the code.
    fn show_code(&mut self) -> Option<Action> {
        if self
            .overlay
            .as_ref()
            .is_some_and(|open| matches!(open, Overlay::Code(_)))
        {
            self.overlay = None;
            return None;
        }

        // The service under the cursor, or the first one with anywhere
        // to go — which is what `kobune url` picks when nothing is named.
        let service = match self.selected_service() {
            Some(service) => Some(service),
            None => self
                .selected()?
                .services
                .iter()
                .find(|service| service.access().is_some()),
        };

        let Some(service) = service else {
            self.went_wrong("no service here has a URL to show".to_string());
            return None;
        };

        if service.access().is_none() {
            self.went_wrong(format!("{} has no URL to show", service.name));
            return None;
        }

        self.show(Overlay::Code(Box::new(service.clone())));
        None
    }

    /// What `doctor` or `env ls` came back with.
    pub fn inspected(&mut self, overlay: Overlay) {
        // Only into the box that is waiting for it. An answer arriving
        // after somebody pressed esc would otherwise reopen it.
        if self
            .overlay
            .as_ref()
            .is_some_and(|open| open.is_like(&overlay))
        {
            self.overlay = Some(overlay);
            self.overlay_scroll = 0;
        }
    }

    fn overlay_page(&self) -> isize {
        isize::try_from(self.overlay_rows).unwrap_or(1).max(1)
    }

    /// As far back as an overlay goes: far enough to put its last row at
    /// the bottom of the box, and no further.
    fn overlay_scrolled_max(&self) -> usize {
        usize::from(self.overlay_content.saturating_sub(self.overlay_rows))
    }

    /// **Clamped, which is the whole of it.** `G` asks to scroll by
    /// [`isize::MAX`], and without a bound that saturates the offset to
    /// a number no amount of pressing `↑` will come back from — the box
    /// drew correctly, because the drawing clamps, and the keys were
    /// dead. [`Logs::scroll`] had the bound; this did not.
    fn scroll_overlay(&mut self, rows: isize) -> Option<Action> {
        self.overlay_scroll = self
            .overlay_scroll
            .saturating_add_signed(rows)
            .min(self.overlay_scrolled_max());
        None
    }

    /// `l`: open the log pane on what the cursor is on, or close it.
    ///
    /// Pressing it again somewhere else moves the stream there rather
    /// than closing, which is what somebody comparing two workspaces
    /// means by it. Pressing it on what is already being followed is the
    /// only way it closes.
    fn toggle_logs(&mut self) -> Option<Action> {
        let subject = self.subject_at_cursor()?;

        if self
            .logs
            .as_ref()
            .is_some_and(|logs| logs.subject == subject)
        {
            return Some(self.close_logs());
        }

        Some(self.open_logs(subject))
    }

    /// `L`: the same pane, with the screen to itself.
    ///
    /// Opens one first where there is none, because wanting a full screen
    /// of logs is not a reason to have to press two keys.
    fn toggle_full(&mut self) -> Option<Action> {
        if self.logs.is_none() {
            let subject = self.subject_at_cursor()?;
            let action = self.open_logs(subject);
            self.logs_full = true;
            return Some(action);
        }

        self.logs_full = !self.logs_full;

        // Nothing else is on screen to be talking to.
        if self.logs_full {
            self.focus = Focus::Logs;
        }

        None
    }

    fn open_logs(&mut self, subject: Subject) -> Action {
        let command = Command::Follow {
            path: subject.path.clone(),
            services: subject.service.iter().cloned().collect(),
        };

        self.logs = Some(Logs::new(subject));
        self.focus = Focus::Logs;

        Action::Ask(command)
    }

    fn close_logs(&mut self) -> Action {
        self.logs = None;
        self.logs_full = false;

        // The pane the keys were talking to has gone.
        if self.focus == Focus::Logs {
            self.focus = Focus::Workspaces;
        }

        Action::Ask(Command::StopFollowing)
    }

    /// What the cursor is on, as something to follow.
    fn subject_at_cursor(&self) -> Option<Subject> {
        let service = self.selected_service().map(|service| service.name.clone());
        let workspace = self.selected()?;

        Some(Subject {
            path: workspace.path.clone(),
            workspace: workspace.display_name().to_string(),
            service,
        })
    }

    /// One line off the stream.
    pub fn on_log(&mut self, service: Option<String>, line: String) {
        if let Some(logs) = &mut self.logs {
            logs.push(LogLine { service, line });
        }
    }

    /// The stream stopped without being asked to.
    pub fn log_ended(&mut self, reason: Option<String>) {
        if let Some(logs) = &mut self.logs {
            logs.ended = Some(match reason {
                Some(reason) => format!("ended: {reason}"),
                None => "ended".to_string(),
            });
        }
    }

    /// `u` and `d`, which differ only in direction.
    ///
    /// **One at a time.** A second operation would put a second stream of
    /// steps into the one line that shows them, and neither would be
    /// readable. The daemon would run both quite happily; this is about
    /// the display.
    fn operate(&mut self, up: bool) -> Option<Action> {
        if self.activity.is_some() {
            return None;
        }

        let workspace = self.selected()?;
        let path = workspace.path.clone();
        let name = workspace.display_name().to_string();

        // A service under the cursor is what is meant; with none, the
        // daemon reads an empty list as all of them.
        let services = match self.selected_service() {
            Some(service) => vec![service.name.clone()],
            None => Vec::new(),
        };

        let subject = match services.first() {
            Some(service) => format!("{name} / {service}"),
            None => name,
        };

        let verb = if up { "starting" } else { "stopping" };
        self.trouble = None;
        self.activity = Some(Activity {
            path: path.clone(),
            label: format!("{verb} {subject}"),
            steps: Vec::new(),
        });

        let command = if up {
            Command::Up { path, services }
        } else {
            Command::Down { path, services }
        };

        Some(Action::Ask(command))
    }

    /// `o`: the way into whatever the cursor is on.
    ///
    /// In the workspace pane that is the first service with a URL, which
    /// is nearly always the one somebody wanted. A service with no way in
    /// — a database — has nothing to open, and saying so beats opening
    /// something else.
    fn open(&mut self) -> Option<Action> {
        let url = match self.selected_service() {
            Some(service) => match service.url.clone() {
                Some(url) => url,
                None => {
                    self.went_wrong(format!("{} has no URL to open", service.name));
                    return None;
                }
            },
            None => {
                let workspace = self.selected()?;
                match workspace
                    .services
                    .iter()
                    .find_map(|service| service.url.clone())
                {
                    Some(url) => url,
                    None => {
                        self.went_wrong("no service here has a URL to open".to_string());
                        return None;
                    }
                }
            }
        };

        Some(Action::Open(url))
    }

    /// Round the panes that are on screen, in the order they are drawn.
    ///
    /// A pane that is not there is stepped over rather than focused: a
    /// cursor in an empty services column, or in a log pane nobody
    /// opened, would be a cursor with nothing under it.
    fn switch_pane(&mut self) {
        // Full screen, there is one pane and it already has the keys.
        if self.logs_are_full() {
            return;
        }

        let has_services = self
            .selected()
            .is_some_and(|workspace| !workspace.services.is_empty());

        match self.focus {
            Focus::Workspaces if has_services => {
                self.selected_service = self
                    .selected()
                    .and_then(|workspace| workspace.services.first())
                    .map(|service| service.name.clone());
                self.focus = Focus::Services;
            }
            Focus::Workspaces | Focus::Services if self.logs.is_some() => {
                self.focus = Focus::Logs;
            }
            Focus::Workspaces => {}
            Focus::Services | Focus::Logs => {
                self.selected_service = None;
                self.focus = Focus::Workspaces;
            }
        }
    }

    /// A screenful, in whichever pane the keys are in.
    fn page(&mut self, direction: isize) {
        let rows = match (self.focus, &self.logs) {
            (Focus::Logs, Some(logs)) => isize::try_from(logs.viewport.rows).unwrap_or(1).max(1),
            // A list of workspaces is drawn on one screen, so a page of
            // it is all of it, and this is the end it was pointed at.
            _ => {
                return self.move_by(if direction < 0 {
                    isize::MIN
                } else {
                    isize::MAX
                });
            }
        };

        self.move_by(direction.saturating_mul(rows));
    }

    fn move_by(&mut self, delta: isize) {
        match self.focus {
            Focus::Workspaces => {
                let Some(index) = step(self.selected_index(), self.workspaces.len(), delta) else {
                    return;
                };

                self.selected = self
                    .workspaces
                    .get(index)
                    .map(|workspace| workspace.path.clone());
            }
            // Up is back through time, which is the opposite sign to a
            // list where up is a smaller index.
            Focus::Logs => {
                if let Some(logs) = &mut self.logs {
                    logs.scroll(delta.saturating_neg());
                }
            }
            Focus::Services => {
                let Some(workspace) = self.selected() else {
                    return;
                };

                let Some(index) = step(
                    self.selected_service_index(),
                    workspace.services.len(),
                    delta,
                ) else {
                    return;
                };

                self.selected_service = workspace
                    .services
                    .get(index)
                    .map(|service| service.name.clone());
            }
        }
    }
}

/// Which workspace the cursor starts on.
///
/// **The one you are standing in.** Every other command infers the
/// workspace from the working directory, and a dashboard opened inside a
/// worktree that showed a different one would be the only thing in the
/// CLI that did not. A `--workspace` wins over it, for the same reason it
/// wins everywhere else.
///
/// The paths are resolved before they are compared: `/tmp` is a symlink
/// on macOS, and a worktree under it is reported by one name and reached
/// by the other. A failure to resolve is not a failure to open — it falls
/// through to the first workspace, which is where this always started.
fn opening_on<'a>(
    workspaces: &'a [WorkspaceInfo],
    at: &Path,
    named: Option<&str>,
) -> Option<&'a WorkspaceInfo> {
    if let Some(named) = named {
        let found = workspaces.iter().find(|workspace| {
            workspace.workspace.as_deref() == Some(named)
                || workspace.display_name() == named
                || (workspace.is_main && named == "main")
        });

        // A name that matches nothing is not quietly ignored in favour of
        // somewhere else: the cursor lands at the top, and the listing
        // beside it shows there is no such workspace.
        if found.is_some() {
            return found;
        }
    }

    let here = at.canonicalize();
    let here = here.as_deref().unwrap_or(at);

    workspaces
        .iter()
        // The longest match, because the main worktree is an ancestor of
        // nothing but itself — unless somebody keeps their worktrees
        // inside it, and then it is an ancestor of all of them.
        .filter(|workspace| {
            let path = workspace.path.canonicalize();
            here.starts_with(path.as_deref().unwrap_or(&workspace.path))
        })
        .max_by_key(|workspace| workspace.path.components().count())
        .or_else(|| workspaces.first())
}

/// The next row, or `None` when there is nowhere to go.
///
/// Stops at the ends rather than wrapping. A list of three workspaces is
/// read at a glance, and a cursor that jumps from the last to the first
/// looks like the list moved.
fn step(current: Option<usize>, len: usize, delta: isize) -> Option<usize> {
    if len == 0 {
        return None;
    }

    // Nowhere yet: the first row, whichever way the key pointed. Falling
    // back to 0 and then stepping would make ↓ land on the second row
    // and ↑ do nothing at all.
    let Some(current) = current else {
        return Some(0);
    };

    let next = current.saturating_add_signed(delta).min(len - 1);

    (next != current).then_some(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    use kobune_api::ServiceScope;
    use kobune_core::ServiceState;

    fn service(name: &str, state: ServiceState) -> ServiceInfo {
        ServiceInfo {
            name: name.into(),
            state,
            reason: None,
            scope: ServiceScope::Workspace,
            url: Some(format!("https://{name}.example.localhost")),
            tunnel_url: None,
            endpoint: None,
            port: None,
            container_id: None,
            image: None,
        }
    }

    fn workspace(name: &str, services: Vec<ServiceInfo>) -> WorkspaceInfo {
        WorkspaceInfo {
            project: "myapp".into(),
            workspace: Some(name.into()),
            branch: format!("feature/{name}"),
            path: PathBuf::from(format!("/repo.wt/{name}")),
            is_main: false,
            services,
        }
    }

    fn listing() -> Vec<WorkspaceInfo> {
        vec![
            workspace(
                "feat-1",
                vec![
                    service("web", ServiceState::Ready),
                    service("api", ServiceState::Stopped),
                ],
            ),
            workspace("feat-2", vec![service("web", ServiceState::Stopped)]),
        ]
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// The main worktree, which is where a repository is checked out.
    fn main_worktree(path: &str) -> WorkspaceInfo {
        WorkspaceInfo {
            project: "myapp".into(),
            workspace: None,
            branch: "main".into(),
            path: PathBuf::from(path),
            is_main: true,
            services: Vec::new(),
        }
    }

    #[test]
    fn opens_on_the_workspace_it_was_run_in() {
        // Every other command infers the workspace from the working
        // directory. A dashboard that showed a different one would be
        // the only thing in the CLI that did not.
        let mut workspaces = vec![main_worktree("/repo")];
        workspaces.extend(listing());

        let app = App::new(workspaces, Path::new("/repo.wt/feat-2/src"), None);
        assert_eq!(app.selected().expect("selected").display_name(), "feat-2");
        assert_eq!(app.focus(), Focus::Workspaces);
    }

    #[test]
    fn the_main_worktree_is_where_it_opens_from_the_repository() {
        let mut workspaces = vec![main_worktree("/repo")];
        workspaces.extend(listing());

        let app = App::new(workspaces, Path::new("/repo/crates/thing"), None);
        assert!(app.selected().expect("selected").is_main);
    }

    #[test]
    fn a_worktree_kept_inside_the_repository_still_wins() {
        // `{repo}.wt/{name}` is the default, not the rule. Under the
        // repository, main is an ancestor of every worktree, and the
        // longest match is the one somebody is standing in.
        let workspaces = vec![
            main_worktree("/repo"),
            WorkspaceInfo {
                path: PathBuf::from("/repo/.wt/feat-1"),
                ..workspace("feat-1", Vec::new())
            },
        ];

        let app = App::new(workspaces, Path::new("/repo/.wt/feat-1"), None);
        assert_eq!(app.selected().expect("selected").display_name(), "feat-1");
    }

    #[test]
    fn a_named_workspace_wins_over_the_directory() {
        let app = App::new(listing(), Path::new("/repo.wt/feat-1"), Some("feat-2"));
        assert_eq!(app.selected().expect("selected").display_name(), "feat-2");
    }

    #[test]
    fn main_can_be_named_even_though_it_has_no_label() {
        let mut workspaces = listing();
        workspaces.push(main_worktree("/repo"));

        let app = App::new(workspaces, Path::new("/elsewhere"), Some("main"));
        assert!(app.selected().expect("selected").is_main);
    }

    #[test]
    fn opens_at_the_top_when_it_is_run_from_nowhere_in_particular() {
        let app = App::new(listing(), Path::new("/elsewhere"), None);
        assert_eq!(app.selected().expect("selected").display_name(), "feat-1");
    }

    #[test]
    fn up_asks_for_the_whole_workspace_from_the_workspace_pane() {
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);

        let action = app.on_key(press(KeyCode::Char('u'))).expect("an action");
        assert_eq!(
            action,
            Action::Ask(Command::Up {
                path: PathBuf::from("/repo.wt/feat-1"),
                services: Vec::new(),
            })
        );
    }

    #[test]
    fn up_asks_for_one_service_from_the_services_pane() {
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);

        app.on_key(press(KeyCode::Tab));
        app.on_key(press(KeyCode::Down));

        let action = app.on_key(press(KeyCode::Char('u'))).expect("an action");
        assert_eq!(
            action,
            Action::Ask(Command::Up {
                path: PathBuf::from("/repo.wt/feat-1"),
                services: vec!["api".to_string()],
            })
        );
    }

    #[test]
    fn a_refresh_does_not_move_the_cursor_to_another_workspace() {
        // The whole reason the selection is a path. A worktree removed in
        // another terminal used to slide the row under the cursor, and
        // the next `d` stopped something nobody was looking at.
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Down));
        assert_eq!(app.selected().expect("selected").display_name(), "feat-2");

        let mut shorter = listing();
        shorter.remove(0);
        app.listing(shorter);

        assert_eq!(app.selected().expect("selected").display_name(), "feat-2");
    }

    #[test]
    fn a_selection_that_is_gone_falls_to_its_neighbour() {
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Down));

        app.listing(vec![workspace("feat-1", vec![])]);

        assert_eq!(app.selected().expect("selected").display_name(), "feat-1");
    }

    #[test]
    fn only_one_operation_runs_at_a_time() {
        // Two streams of steps into the one line that shows them would
        // leave neither readable.
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        assert!(app.on_key(press(KeyCode::Char('u'))).is_some());
        assert!(app.on_key(press(KeyCode::Char('d'))).is_none());
    }

    #[test]
    fn an_empty_listing_survives_every_key() {
        let mut app = App::new(Vec::new(), Path::new("/repo.wt/feat-1"), None);

        for code in [
            KeyCode::Down,
            KeyCode::Up,
            KeyCode::Tab,
            KeyCode::Char('u'),
            KeyCode::Char('d'),
            KeyCode::Char('o'),
        ] {
            app.on_key(press(code));
        }

        assert!(app.selected().is_none());
        assert!(!app.is_done());
    }

    #[test]
    fn a_cursor_that_is_nowhere_lands_on_the_first_row() {
        // Either way. Treating "nowhere" as row 0 and then stepping would
        // have ↓ skip the first row and ↑ do nothing.
        assert_eq!(step(None, 3, 1), Some(0));
        assert_eq!(step(None, 3, -1), Some(0));
        assert_eq!(step(None, 0, 1), None);
    }

    #[test]
    fn the_cursor_stops_at_the_ends() {
        // Wrapping in a list of two reads as the list having moved.
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Up));
        assert_eq!(app.selected().expect("selected").display_name(), "feat-1");

        app.on_key(press(KeyCode::Down));
        app.on_key(press(KeyCode::Down));
        assert_eq!(app.selected().expect("selected").display_name(), "feat-2");
    }

    #[test]
    fn focus_stays_put_when_there_are_no_services_to_move_to() {
        let mut app = App::new(
            vec![workspace("feat-1", vec![])],
            Path::new("/repo.wt/feat-1"),
            None,
        );
        app.on_key(press(KeyCode::Tab));

        assert_eq!(app.focus(), Focus::Workspaces);
    }

    /// A log pane of this size, and an overlay with room to spare.
    fn pane(columns: u16, rows: u16) -> Measured {
        Measured {
            log_columns: columns,
            log_rows: rows,
            overlay_rows: 20,
            overlay_content: 0,
        }
    }

    /// What the box on top is about, or nothing.
    fn showing(app: &App) -> Option<&'static str> {
        app.overlay().map(Overlay::kind)
    }

    #[test]
    fn the_key_that_opened_an_overlay_closes_it() {
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);

        app.on_key(press(KeyCode::Char('?')));
        assert_eq!(showing(&app), Some("keys"));

        app.on_key(press(KeyCode::Char('?')));
        assert_eq!(showing(&app), None);
        assert!(!app.is_done(), "it put the list away, not the dashboard");
    }

    #[test]
    fn one_overlay_gives_way_to_the_next() {
        // Pressing `c` while the keys are up means "show me the checks",
        // not "close this and press it again".
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);

        app.on_key(press(KeyCode::Char('?')));
        app.on_key(press(KeyCode::Char('c')));

        assert_eq!(showing(&app), Some("the checks"));
    }

    #[test]
    fn an_overlay_takes_the_keys_that_move_through_it() {
        // `kobune doctor` on a machine with something wrong is taller
        // than the screen. Arrow keys that moved the cursor behind it
        // would leave no way to read the bottom.
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Char('?')));

        // Taller than the box it is in, or there is nothing to scroll.
        app.resize(Measured {
            overlay_rows: 6,
            overlay_content: 30,
            ..pane(80, 10)
        });

        app.on_key(press(KeyCode::Down));
        app.on_key(press(KeyCode::Down));

        assert_eq!(app.overlay_scroll(), 2);
        assert_eq!(
            app.selected().expect("selected").display_name(),
            "feat-1",
            "the cursor behind it did not move"
        );
    }

    #[test]
    fn the_end_of_an_overlay_is_somewhere_it_can_come_back_from() {
        // `G` asks to scroll by `isize::MAX`. Unclamped that saturated
        // the offset to a number no amount of pressing `↑` returned
        // from, and the box drew correctly the whole time because the
        // drawing does its own clamping.
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Char('?')));
        app.resize(Measured {
            overlay_rows: 6,
            overlay_content: 30,
            ..pane(80, 10)
        });

        app.on_key(press(KeyCode::Char('G')));
        assert_eq!(app.overlay_scroll(), 24, "the last row at the bottom");

        app.on_key(press(KeyCode::Up));
        assert_eq!(app.overlay_scroll(), 23, "and one press comes back");
    }

    #[test]
    fn a_window_that_grew_lets_an_overlay_stop_scrolling() {
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Char('?')));
        app.resize(Measured {
            overlay_rows: 6,
            overlay_content: 30,
            ..pane(80, 10)
        });
        app.on_key(press(KeyCode::Char('G')));

        app.resize(Measured {
            overlay_rows: 40,
            overlay_content: 30,
            ..pane(80, 10)
        });

        assert_eq!(app.overlay_scroll(), 0, "all of it is on screen now");
    }

    #[test]
    fn a_log_pane_that_grew_starts_following_again() {
        // It said `paused · 15 below` with the whole buffer on screen
        // and nothing below it.
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Char('l')));
        app.resize(pane(80, 5));

        for n in 0..20 {
            app.on_log(None, format!("line {n}"));
        }
        app.on_key(press(KeyCode::Char('g')));
        assert!(!app.logs().expect("open").following());

        app.resize(pane(80, 40));

        let logs = app.logs().expect("open");
        assert_eq!(logs.behind(), 0);
        assert!(logs.following(), "nothing is below it any more");
    }

    #[test]
    fn esc_puts_an_overlay_away_and_q_leaves() {
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);

        app.on_key(press(KeyCode::Char('?')));
        app.on_key(press(KeyCode::Esc));
        assert_eq!(showing(&app), None);
        assert!(!app.is_done());

        app.on_key(press(KeyCode::Char('?')));
        app.on_key(press(KeyCode::Char('q')));
        assert!(app.is_done(), "`q` leaves from anywhere");
    }

    #[test]
    fn a_key_with_nothing_to_do_with_the_overlay_still_does_its_job() {
        // The one thing worse than an overlay you cannot scroll is one
        // you cannot get out from behind.
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Char('?')));

        assert!(matches!(
            app.on_key(press(KeyCode::Char('u'))),
            Some(Action::Ask(Command::Up { .. }))
        ));
        assert_eq!(showing(&app), None);
    }

    #[test]
    fn the_checks_are_asked_for_and_waited_on() {
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);

        assert_eq!(
            app.on_key(press(KeyCode::Char('c'))),
            Some(Action::Ask(Command::Checks {
                path: PathBuf::from("/repo.wt/feat-1"),
            }))
        );

        // Up before the answer, so a slow daemon looks slow rather than
        // like a key that did nothing.
        assert_eq!(showing(&app), Some("the checks"));
    }

    #[test]
    fn the_environment_asked_for_is_the_one_under_the_cursor() {
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Tab));

        assert_eq!(
            app.on_key(press(KeyCode::Char('e'))),
            Some(Action::Ask(Command::Env {
                path: PathBuf::from("/repo.wt/feat-1"),
                service: Some("web".to_string()),
            }))
        );
    }

    #[test]
    fn an_answer_only_lands_in_the_box_that_was_waiting_for_it() {
        // Otherwise a reply arriving a moment after esc reopens the
        // overlay somebody has just dismissed.
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Char('c')));
        app.on_key(press(KeyCode::Esc));

        app.inspected(Overlay::Failed {
            what: "the checks",
            reason: "no daemon".into(),
        });

        assert_eq!(showing(&app), None);
    }

    #[test]
    fn a_failure_replaces_the_box_that_was_waiting() {
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Char('c')));

        app.inspected(Overlay::Failed {
            what: "the checks",
            reason: "no daemon".into(),
        });

        assert!(matches!(app.overlay(), Some(Overlay::Failed { .. })));
    }

    #[test]
    fn the_code_needs_no_daemon_because_the_listing_has_the_url() {
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);

        assert_eq!(app.on_key(press(KeyCode::Char('Q'))), None);
        assert_eq!(showing(&app), Some("code"));
    }

    #[test]
    fn the_code_says_so_when_there_is_nothing_to_show() {
        let mut app = App::new(
            vec![workspace(
                "feat-1",
                vec![ServiceInfo {
                    url: None,
                    ..service("db", ServiceState::Ready)
                }],
            )],
            Path::new("/repo.wt/feat-1"),
            None,
        );

        app.on_key(press(KeyCode::Char('Q')));
        assert_eq!(showing(&app), None);
        assert!(app.trouble().is_some());
    }

    #[test]
    fn ctrl_c_leaves() {
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));

        assert!(app.is_done());
    }

    #[test]
    fn a_service_reaching_ready_shows_at_once() {
        // The listing is re-read every three seconds, and the three
        // seconds after pressing `u` are exactly when somebody is
        // watching. The daemon has already said so by then.
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Char('u')));

        assert_eq!(
            app.selected().expect("selected").services[1].state,
            ServiceState::Stopped
        );

        app.on_event(&Event::ServiceState {
            service: "api".into(),
            state: ServiceState::Ready,
            reason: None,
        });

        assert_eq!(
            app.selected().expect("selected").services[1].state,
            ServiceState::Ready
        );
    }

    #[test]
    fn a_state_change_lands_on_the_workspace_it_was_asked_about() {
        // Two workspaces run a service of the same name, and the event
        // names the service alone — the request is what says which
        // workspace it was about.
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Down));
        app.on_key(press(KeyCode::Char('u')));

        app.on_event(&Event::ServiceState {
            service: "web".into(),
            state: ServiceState::Ready,
            reason: None,
        });

        assert_eq!(
            app.workspaces()[1].services[0].state,
            ServiceState::Ready,
            "the one that was asked about"
        );
        assert_eq!(
            app.workspaces()[0].services[0].state,
            ServiceState::Ready,
            "the other one is untouched — it started out ready"
        );
        assert_eq!(
            app.workspaces()[0].services[1].state,
            ServiceState::Stopped,
            "and nothing else moved"
        );
    }

    #[test]
    fn a_state_change_with_nothing_running_is_ignored() {
        // There is no request to say which workspace it was about.
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);

        app.on_event(&Event::ServiceState {
            service: "api".into(),
            state: ServiceState::Ready,
            reason: None,
        });

        assert_eq!(
            app.selected().expect("selected").services[1].state,
            ServiceState::Stopped
        );
    }

    #[test]
    fn a_failure_arrives_with_its_reason_beside_it() {
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Char('u')));

        app.on_event(&Event::ServiceState {
            service: "api".into(),
            state: ServiceState::failed(String::new()),
            reason: Some("port 3000 is in use".into()),
        });

        let service = &app.selected().expect("selected").services[1];
        assert_eq!(service.reason.as_deref(), Some("port 3000 is in use"));
    }

    #[test]
    fn a_failed_step_is_kept_where_it_can_be_read() {
        // The operation carries on; the reason has to outlive the step,
        // or it is gone by the time anybody looks up.
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Char('u')));

        app.on_event(&Event::Step {
            id: "start-web".into(),
            label: "starting web".into(),
            status: StepStatus::Failed {
                reason: "port 3000 is in use".into(),
            },
        });

        assert_eq!(
            app.trouble(),
            Some("starting web: port 3000 is in use"),
            "the reason has to survive the step"
        );
    }

    #[test]
    fn steps_arrive_and_settle() {
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Char('u')));

        app.on_event(&Event::Step {
            id: "pull".into(),
            label: "pulling node:22-alpine".into(),
            status: StepStatus::Started,
        });
        app.on_event(&Event::Step {
            id: "pull".into(),
            label: "pulling node:22-alpine".into(),
            status: StepStatus::Progress {
                message: "layer 2/5".into(),
            },
        });

        let activity = app.activity().expect("running");
        assert_eq!(activity.steps.len(), 1);
        assert_eq!(activity.steps[0].detail.as_deref(), Some("layer 2/5"));

        app.on_event(&Event::Step {
            id: "pull".into(),
            label: "pulling node:22-alpine".into(),
            status: StepStatus::Done,
        });
        assert!(app.activity().expect("running").steps.is_empty());

        app.settled(Ok(()));
        assert!(app.activity().is_none());
    }

    #[test]
    fn l_follows_what_the_cursor_is_on() {
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);

        assert_eq!(
            app.on_key(press(KeyCode::Char('l'))),
            Some(Action::Ask(Command::Follow {
                path: PathBuf::from("/repo.wt/feat-1"),
                services: Vec::new(),
            })),
            "the workspace pane means every service"
        );

        assert!(app.logs().is_some());
        assert_eq!(app.focus(), Focus::Logs);
    }

    #[test]
    fn l_on_a_service_follows_that_one() {
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Tab));

        assert_eq!(
            app.on_key(press(KeyCode::Char('l'))),
            Some(Action::Ask(Command::Follow {
                path: PathBuf::from("/repo.wt/feat-1"),
                services: vec!["web".to_string()],
            }))
        );
    }

    #[test]
    fn l_on_what_is_already_followed_closes_the_pane() {
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Char('l')));

        assert_eq!(
            app.on_key(press(KeyCode::Char('l'))),
            Some(Action::Ask(Command::StopFollowing))
        );
        assert!(app.logs().is_none());
        assert_eq!(app.focus(), Focus::Workspaces, "the pane it was in is gone");
    }

    #[test]
    fn the_pane_stays_on_what_it_was_opened_on() {
        // Following the cursor would tear the stream down and open
        // another on every keypress, and lose everything scrolled back
        // to. A second `l` is how the subject changes.
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Char('l')));

        app.on_key(press(KeyCode::Tab));
        app.on_key(press(KeyCode::Down));

        let subject = &app.logs().expect("open").subject;
        assert_eq!(subject.workspace, "feat-1");
        assert_eq!(subject.service, None);
    }

    #[test]
    fn l_somewhere_else_moves_the_pane_rather_than_closing_it() {
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Char('l')));

        // Back to the workspaces, down one, and ask again.
        app.on_key(press(KeyCode::Tab));
        app.on_key(press(KeyCode::Down));

        assert_eq!(
            app.on_key(press(KeyCode::Char('l'))),
            Some(Action::Ask(Command::Follow {
                path: PathBuf::from("/repo.wt/feat-2"),
                services: Vec::new(),
            }))
        );
        assert_eq!(app.logs().expect("open").subject.workspace, "feat-2");
    }

    #[test]
    fn scrolling_stops_following_and_g_starts_again() {
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Char('l')));
        app.resize(pane(80, 10));

        for n in 0..50 {
            app.on_log(Some("web".into()), format!("line {n}"));
        }
        assert!(app.logs().expect("open").following());

        app.on_key(press(KeyCode::Up));
        assert!(!app.logs().expect("open").following());
        assert_eq!(app.logs().expect("open").behind(), 1);

        // A page is the pane, so it is the pane that has to be asked.
        app.on_key(press(KeyCode::PageUp));
        assert_eq!(app.logs().expect("open").behind(), 11);

        app.on_key(press(KeyCode::Char('G')));
        assert!(app.logs().expect("open").following(), "back to the tail");
    }

    #[test]
    fn a_line_wider_than_the_pane_is_wrapped_rather_than_cut() {
        // It used to lose its tail without saying so, and the lines that
        // overflow are the ones worth reading: a stack trace, a failing
        // assertion, a URL.
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Char('l')));
        app.resize(pane(20, 10));

        app.on_log(None, "0123456789012345678901234567890123456789".into());

        let rows = app.logs().expect("open").rows();
        let text: String = rows
            .iter()
            .flat_map(|row| row.spans.iter().map(|span| span.content.as_ref()))
            .collect();

        assert!(rows.len() >= 2, "it took more than one row: {rows:?}");
        assert!(
            text.contains("0123456789012345678901234567890123456789"),
            "every character is still there: {text:?}"
        );
    }

    #[test]
    fn a_wrapped_line_is_indented_under_its_own_service() {
        // Otherwise the second row reads as a line nobody wrote.
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Char('l')));
        app.resize(pane(20, 10));

        app.on_log(Some("web".into()), "a".repeat(30));

        // Twenty columns, less the three the column takes and the two
        // beside it: fifteen a row, so thirty is two of them.
        let rows = app.logs().expect("open").rows();
        assert_eq!(rows.len(), 2, "the column narrows the text: {rows:?}");

        let first: String = rows[0].spans.iter().map(|s| s.content.as_ref()).collect();
        let second: String = rows[1].spans.iter().map(|s| s.content.as_ref()).collect();

        assert!(first.starts_with("web  "), "labelled: {first:?}");
        assert!(second.starts_with("     "), "indented under it: {second:?}");
        assert!(!second.contains("web"), "and not labelled twice");
    }

    #[test]
    fn a_page_is_the_pane_it_is_drawn_in() {
        // The pane's height is not something the state can guess, and a
        // page that was not a screenful would be a page in name only.
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Char('l')));
        app.resize(pane(80, 30));

        for n in 0..200 {
            app.on_log(None, format!("line {n}"));
        }

        app.on_key(press(KeyCode::PageUp));
        assert_eq!(app.logs().expect("open").behind(), 30);
    }

    #[test]
    fn the_oldest_row_is_as_far_back_as_it_goes() {
        // Past that and the pane would be scrolled into blank space.
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Char('l')));
        app.resize(pane(80, 10));

        for n in 0..25 {
            app.on_log(None, format!("line {n}"));
        }

        app.on_key(press(KeyCode::Char('g')));

        let logs = app.logs().expect("open");
        assert_eq!(logs.behind(), 15, "25 rows, a pane of 10");

        let rows = logs.rows();
        let first: String = rows[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(first, "line 0", "the oldest is at the top: {rows:?}");
    }

    #[test]
    fn lines_arriving_do_not_slide_what_is_being_read() {
        // Scrolled up, the view holds still. Otherwise a chatty service
        // makes it impossible to read anything but the tail.
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Char('l')));

        for n in 0..20 {
            app.on_log(None, format!("line {n}"));
        }
        app.on_key(press(KeyCode::Up));

        let before = app.logs().expect("open").rows();

        for n in 20..30 {
            app.on_log(None, format!("line {n}"));
        }

        let after = app.logs().expect("open").rows();

        assert_eq!(before, after);
    }

    #[test]
    fn the_buffer_has_a_bottom() {
        // A service in a restart loop writes the same line for as long
        // as the dashboard is open.
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Char('l')));

        for n in 0..(MAX_LOG_LINES + 500) {
            app.on_log(None, format!("line {n}"));
        }

        let logs = app.logs().expect("open");
        let rows = logs.rows();
        let last: String = rows
            .last()
            .expect("a row")
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();

        assert_eq!(last, format!("line {}", MAX_LOG_LINES + 499));
        assert_eq!(logs.lines.len(), MAX_LOG_LINES);
    }

    #[test]
    fn esc_backs_out_one_step_at_a_time() {
        // Full screen, then the pane, then the dashboard. `q` is the one
        // that always leaves.
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Char('L')));
        assert!(app.logs_are_full());

        app.on_key(press(KeyCode::Esc));
        assert!(!app.logs_are_full());
        assert!(app.logs().is_some());
        assert!(!app.is_done());

        app.on_key(press(KeyCode::Esc));
        assert!(app.logs().is_none());
        assert!(!app.is_done());

        app.on_key(press(KeyCode::Esc));
        assert!(app.is_done());
    }

    #[test]
    fn a_full_screen_of_logs_is_one_key_from_anywhere() {
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);

        assert!(matches!(
            app.on_key(press(KeyCode::Char('L'))),
            Some(Action::Ask(Command::Follow { .. }))
        ));
        assert!(app.logs_are_full());
        assert_eq!(app.focus(), Focus::Logs);
    }

    #[test]
    fn the_pane_cycle_steps_over_what_is_not_on_screen() {
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);

        app.on_key(press(KeyCode::Tab));
        assert_eq!(app.focus(), Focus::Services);

        // No log pane yet, so the cycle is two long.
        app.on_key(press(KeyCode::Tab));
        assert_eq!(app.focus(), Focus::Workspaces);

        app.on_key(press(KeyCode::Char('l')));
        app.on_key(press(KeyCode::Tab));
        assert_eq!(app.focus(), Focus::Workspaces);
        app.on_key(press(KeyCode::Tab));
        assert_eq!(app.focus(), Focus::Services);
        app.on_key(press(KeyCode::Tab));
        assert_eq!(app.focus(), Focus::Logs, "now there are three");
    }

    #[test]
    fn a_stream_that_stops_on_its_own_says_so() {
        // A pane that has simply gone quiet looks exactly the same as
        // one whose stream died.
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Char('l')));

        app.log_ended(Some("the daemon went away".into()));

        assert_eq!(
            app.logs().expect("open").ended.as_deref(),
            Some("ended: the daemon went away")
        );
    }

    #[test]
    fn u_still_means_the_service_the_cursor_is_on_while_a_log_is_open() {
        // `l` moves the keys into the log pane, but the services cursor
        // is still where it was and is still what is drawn.
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Tab));
        app.on_key(press(KeyCode::Char('l')));
        assert_eq!(app.focus(), Focus::Logs);

        assert_eq!(
            app.on_key(press(KeyCode::Char('u'))),
            Some(Action::Ask(Command::Up {
                path: PathBuf::from("/repo.wt/feat-1"),
                services: vec!["web".to_string()],
            }))
        );
    }

    #[test]
    fn open_takes_the_url_the_cursor_is_on() {
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Tab));
        app.on_key(press(KeyCode::Down));

        assert_eq!(
            app.on_key(press(KeyCode::Char('o'))),
            Some(Action::Open("https://api.example.localhost".into()))
        );
    }

    #[test]
    fn open_says_so_when_there_is_nothing_to_open() {
        let mut app = App::new(
            vec![workspace(
                "feat-1",
                vec![ServiceInfo {
                    url: None,
                    ..service("db", ServiceState::Ready)
                }],
            )],
            Path::new("/repo.wt/feat-1"),
            None,
        );

        assert!(app.on_key(press(KeyCode::Char('o'))).is_none());
        assert!(app.trouble().is_some(), "it has to say why nothing opened");
    }
}
