//! Everything a person reads.
//!
//! The daemon holds no pre-formatted strings at all — the GUI has to be
//! able to build its own display from the same data (`docs/DESIGN.md` §3).
//! This is the CLI's half of that bargain, and it is drawn with ratatui:
//! [`views`] turn a response into widgets, [`panel`] gives them all one
//! shape, and [`surface`] puts the result on the terminal.
//!
//! Nothing here reaches for the screen directly, and nothing knows whether
//! it is being printed once or repainted sixty times a second. That is the
//! part worth keeping: when `minato` grows a full-screen mode, the views
//! go into a [`ratatui::Frame`] unchanged.
//!
//! Machine-facing output — `--json`, and the container output `logs` and
//! `exec` pass through — is not here. See [`crate::output`].

mod panel;
mod progress;
mod qr;
mod surface;
mod theme;
mod views;

use std::path::Path;

use minato_api::{Diagnostics, EnvInfo, Pong, TunnelInfo, WorkspaceInfo};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

pub use progress::Progress;
pub use views::{SetupOutcome, SetupStep};

use surface::Surface;
pub use surface::window;

/// Whether the screen can be drawn on at all.
///
/// The same question every view asks before it decides between a frame
/// and plain text, so `TERM=dumb` — where escape sequences arrive as
/// literal text rather than as movement — is a pipe here too.
pub fn is_interactive() -> bool {
    Surface::stdout().is_interactive()
}

/// Something that can be drawn, and that knows how big it wants to be.
///
/// The second half is what a [`ratatui::Widget`] leaves to its caller: on
/// a full screen the area is simply given. Printing once, there is no
/// screen to fill — the view has to say how much of one it needs.
pub trait View {
    /// The width this view would like. It is granted whenever the terminal
    /// is wide enough, so a frame ends up drawn around the content rather
    /// than stretched across the window.
    fn preferred_width(&self) -> u16;

    fn height(&self, width: u16) -> u16;

    fn render(&self, area: Rect, buf: &mut Buffer);
}

/// The state of one workspace: `status`, and the tail of `up` / `down` /
/// `new`.
pub fn workspace(info: &WorkspaceInfo) {
    Surface::stdout().print(|decor| views::workspace(info, decor));
}

/// `ls`.
pub fn workspaces(list: &[WorkspaceInfo]) {
    Surface::stdout().print(|decor| views::workspaces(list, decor));
}

/// `url` with no service named: the whole list.
pub fn urls(info: &WorkspaceInfo, qr: bool) {
    Surface::stdout().print(|decor| views::urls(info, qr, decor));
}

/// `url <service> --qr`: the one URL, as a code to photograph.
pub fn url(service: &minato_api::ServiceInfo) {
    Surface::stdout().print(|decor| views::url(service, decor));
}

/// `doctor`.
pub fn diagnostics(report: &Diagnostics) {
    Surface::stdout().print(|decor| views::diagnostics(report, decor));
}

/// `setup` with nowhere to ask: the commands, to run by hand.
pub fn setup(steps: &[SetupStep], undo: &[String], restart_needed: bool) {
    Surface::stdout().print(|decor| views::setup(steps, undo, restart_needed, decor));
}

/// What an interactive `setup` is about to offer, before it offers any of
/// it.
pub fn setup_plan(steps: &[SetupStep]) {
    Surface::stdout().print(|decor| views::setup_plan(steps, decor));
}

/// One step, on the way to being asked about.
pub fn setup_step(number: usize, total: usize, step: &SetupStep) {
    // A blank line first: each of these is a question of its own, and one
    // run of text is the wrong shape for that.
    let mut lines = vec![Line::raw("")];
    lines.extend(views::setup_step_lines(number, total, step));

    Surface::stdout().print(move |_| Loose(lines));
}

/// What that step came to.
pub fn setup_outcome(outcome: SetupOutcome) {
    Surface::stdout().print(|_| Loose(vec![views::setup_outcome_line(outcome)]));
}

/// Where the whole walk left the machine.
pub fn setup_done(
    steps: &[SetupStep],
    outcomes: &[SetupOutcome],
    undo: &[String],
    restart_needed: bool,
) {
    Surface::stdout()
        .print(|decor| views::setup_done(steps, outcomes, undo, restart_needed, decor));
}

/// `env ls`, and what `env set` / `env unset` leave behind.
pub fn env(entries: &[EnvInfo]) {
    Surface::stdout().print(|decor| views::env(entries, decor));
}

/// Why a value is shown as written, in words.
pub use views::{unsettled_reason, unsettled_remedy};

/// `tunnel status`, `tunnel enable`, `tunnel disable`.
pub fn tunnel(info: &TunnelInfo) {
    Surface::stdout().print(|decor| views::tunnel(info, decor));
}

/// `ping`, and `daemon status` when there is a daemon to report on.
pub fn daemon(pong: &Pong, socket: Option<&Path>) {
    Surface::stdout().print(|decor| views::daemon(pong, socket, decor));
}

/// `daemon status` when there is not.
pub fn daemon_stopped() {
    Surface::stdout().print(views::daemon_stopped);
}

/// What `uninstall` is about to do, before it does any of it.
pub fn uninstall_plan(
    plan: &crate::uninstall::Plan,
    daemon: Result<&minato_api::PurgeReport, &String>,
    dry_run: bool,
) {
    Surface::stdout().print(|decor| views::uninstall_plan(plan, daemon, dry_run, decor));
}

/// What it managed, and what is left to run by hand.
pub fn uninstall_done(failures: &[String], remaining: &[crate::uninstall::Privileged]) {
    Surface::stdout().print(|decor| views::uninstall_done(failures, remaining, decor));
}

/// A command that did one thing, and what to do next.
pub fn done(title: &'static str, facts: &[(&'static str, String)], next: Vec<Line<'static>>) {
    Surface::stdout().print(|decor| views::done(title, facts, next.clone(), decor));
}

/// `run this` — the shape every suggestion in the CLI takes.
pub fn hint(text: &str, command: &str) -> Line<'static> {
    views::hint(text, command)
}

/// A remark of the CLI's own, with no command attached.
pub fn note(text: &str) -> Line<'static> {
    views::note(text)
}

/// A follow-up step: the state of the machine, and the command that
/// settles it.
///
/// One shape for the lot of them, wherever they are printed — the panel
/// `update` leaves behind and the notice a new build prints on its first
/// run both read as a list of the same kind of thing.
pub fn step(reason: &str, command: &str) -> Line<'static> {
    hint(&format!("{reason}, so run"), command)
}

/// A heading and the lines under it.
///
/// For what a command has to say *besides* what it did — the keys a
/// conversion could not carry, and why. A panel would compete with the
/// one above it; a bare list would not say what it was a list of.
pub fn note_lines(heading: &str, lines: &[String]) {
    let mut out = vec![Line::from(vec![
        Span::styled("› ", theme::muted()),
        Span::styled(heading.to_string(), theme::subject()),
    ])];

    out.extend(lines.iter().map(|line| {
        Line::from(vec![
            Span::raw("  "),
            Span::styled(line.clone(), theme::muted()),
        ])
    }));

    Surface::stdout().print(|_| Loose(out.clone()));
}

/// A command that did what it was asked and has nothing to report.
///
/// `rm` is the whole of it: a panel with a title and no contents would say
/// less than one line does.
pub fn confirm(text: &str) {
    Surface::stdout().print(|_| {
        Loose(vec![Line::from(vec![
            Span::styled("✓ ", theme::good()),
            Span::raw(text.to_string()),
        ])])
    });
}

/// One undecorated line.
///
/// `url` and `env get` exist to be piped — `curl "$(minato url web)"` —
/// so what they print is the value and nothing else.
pub fn value(text: &str) {
    Surface::stdout().line(text);
}

/// An error, always with its hint when there is one.
///
/// On stderr, so `$(minato url web)` never picks it up.
pub fn error(message: &str, hint: Option<&str>) {
    let mut lines = vec![Line::from(vec![
        Span::styled("✗ ", theme::bad()),
        Span::styled("error", theme::bad()),
        Span::styled(": ", theme::muted()),
        Span::raw(message.to_string()),
    ])];

    if let Some(hint) = hint {
        lines.push(Line::from(vec![
            Span::styled("  hint: ", theme::muted()),
            Span::raw(hint.to_string()),
        ]));
    }

    Surface::stderr().print(|_| Loose(lines.clone()));
}

/// A note that stands on its own, with no panel around it.
///
/// On stderr: the once-a-day update notice goes through here, and a line
/// about a new build turning up inside output someone is parsing would be
/// a bug rather than a nuisance.
pub fn notice(lines: Vec<Line<'static>>) {
    Surface::stderr().print(|_| Loose(lines.clone()));
}

/// Lines with nothing drawn around them.
struct Loose(Vec<Line<'static>>);

impl View for Loose {
    fn preferred_width(&self) -> u16 {
        self.0
            .iter()
            .map(|line| u16::try_from(line.width()).unwrap_or(u16::MAX))
            .max()
            .unwrap_or(0)
    }

    // Wrapped, on the same reasoning a panel wraps: what comes through
    // here is a `sudo …` line someone is about to agree to, and cutting it
    // off at the window's edge hides the half being agreed to.
    fn height(&self, width: u16) -> u16 {
        u16::try_from(panel::wrap(&self.0, width).len()).unwrap_or(u16::MAX)
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        for (index, line) in panel::wrap(&self.0, area.width).into_iter().enumerate() {
            let Some(y) = area.y.checked_add(u16::try_from(index).unwrap_or(u16::MAX)) else {
                return;
            };
            if y >= area.bottom() {
                return;
            }
            line.render(Rect::new(area.x, y, area.width, 1), buf);
        }
    }
}

#[cfg(test)]
pub mod test_support {
    use super::*;

    /// Draws a view and returns what would reach a terminal that cannot
    /// show colour — which is what the assertions are about.
    pub fn render<V: View>(view: &V) -> String {
        render_at(view, view.preferred_width().max(1))
    }

    /// The same, at a width the view did not choose.
    ///
    /// **What a narrow terminal does to it.** At its preferred width
    /// nothing ever wraps, so an assertion made there cannot fail for the
    /// thing a person on 80 columns actually sees.
    pub fn render_at<V: View>(view: &V, width: u16) -> String {
        let height = view.height(width).max(1);

        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        view.render(area, &mut buffer);

        super::surface::render_to_string(&buffer, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_support::render;

    #[test]
    fn an_error_carries_its_hint() {
        let view = Loose(vec![
            Line::raw("cannot reach the daemon"),
            Line::raw("  hint: minato daemon start"),
        ]);

        let text = render(&view);
        assert!(text.contains("cannot reach the daemon"), "got:\n{text}");
        assert!(text.contains("minato daemon start"), "got:\n{text}");
    }

    #[test]
    fn loose_lines_have_nothing_drawn_round_them() {
        // An error is read in a hurry and often pasted into an issue; a
        // frame would only be in the way.
        let text = render(&Loose(vec![Line::raw("error: nope")]));
        assert_eq!(text, "error: nope\n");
    }

    #[test]
    fn a_step_reads_as_a_state_and_the_command_that_settles_it() {
        // The state first, because it is what decides whether the command
        // is worth running at all — someone who has already restarted
        // their daemon should be able to stop reading at the comma.
        let text = render(&Loose(vec![step(
            "the daemon is still the previous build",
            "minato daemon restart",
        )]));

        assert_eq!(
            text.trim_end(),
            "› the daemon is still the previous build, so run minato daemon restart"
        );
    }
}
