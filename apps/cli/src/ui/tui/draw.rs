//! The dashboard, drawn.
//!
//! A pure function of [`App`] onto a [`Frame`]: nothing is read, nothing
//! is asked for, and the same state always produces the same screen.
//! That is what lets it be asserted on against a
//! [`ratatui::backend::TestBackend`] rather than by looking at a
//! terminal.
//!
//! The right-hand pane is the point of the whole exercise. It is
//! [`views::workspace`] — the function `kobune status` prints with —
//! handed a [`Rect`] instead of a buffer to fill, through [`Framed`].
//! See `docs/DESIGN.md` §3.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Widget};
use unicode_width::UnicodeWidthStr as _;

use crate::ui::panel::{Grid, Panel};
use crate::ui::theme::Decor;
use crate::ui::{Cursor, Framed, View, progress, theme, views};

use super::app::{App, Focus, Logs, Measured, Overlay};
use super::text::{fit, pad};

/// Narrower than this and the names are gone, however little room the
/// window leaves.
const MIN_LIST_WIDTH: u16 = 18;

/// The rows below the panes: a rule, what is happening, and the keys.
const FOOTER_HEIGHT: u16 = 3;

/// Fewer rows than this and a log pane says less than the space it takes
/// from the panes above it.
const MIN_LOG_HEIGHT: u16 = 5;

pub fn draw(app: &App, frame: &mut Frame) {
    let outer = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::frame())
        .title(title(app))
        // For the reason `Decor::block` gives: the title is drawn over
        // the border and would otherwise be the colour of it.
        .title_style(Style::new().fg(Color::Reset))
        .padding(Padding::horizontal(1));

    let inner = outer.inner(frame.area());
    frame.render_widget(outer, frame.area());

    let [body, rule, activity, keys] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    if app.logs_are_full() {
        // Nothing else is worth a row: somebody asked for the logs and
        // the whole screen.
        logs(app, frame, body);
    } else {
        let (above, below) = match app.logs() {
            Some(_) => {
                let [above, below] = Layout::vertical([
                    Constraint::Fill(1),
                    Constraint::Length(log_pane_height(body.height)),
                ])
                .areas(body);

                (above, Some(below))
            }
            None => (body, None),
        };

        if app.workspaces().is_empty() {
            // The panel `kobune ls` prints when there is nothing yet,
            // which already says what to do about it.
            frame.render_widget(Framed(&views::workspaces(&[], Decor::BARE)), above);
        } else {
            panes(app, frame, above);
        }

        if let Some(below) = below {
            logs(app, frame, below);
        }
    }

    frame.render_widget(
        Block::new()
            .borders(Borders::TOP)
            .border_style(theme::frame()),
        rule,
    );
    frame.render_widget(activity_line(app), activity);
    frame.render_widget(key_line(app), keys);

    if let Some(showing) = app.overlay() {
        overlay(app, showing, frame);
    }
}

fn title(app: &App) -> Line<'static> {
    let mut spans = vec![Span::raw(" "), Span::styled("kobune", theme::subject())];

    let project = app.project();
    if !project.is_empty() {
        spans.push(Span::styled(" ─ ", theme::secondary()));
        spans.push(Span::styled(project.to_string(), theme::subject()));
    }

    spans.push(Span::raw(" "));
    Line::from(spans)
}

/// The list on the left, a rule, and one workspace in full on the right.
fn panes(app: &App, frame: &mut Frame, body: Rect) {
    let width = list_width(app, body.width);

    let [left, gap, right] = Layout::horizontal([
        Constraint::Length(width),
        Constraint::Length(3),
        Constraint::Fill(1),
    ])
    .areas(body);

    frame.render_widget(list(app, width, left.height), left);

    // Down the middle of the gap, so there is a space either side of it.
    if gap.width == 3 {
        frame.render_widget(
            Block::new()
                .borders(Borders::LEFT)
                .border_style(theme::frame()),
            Rect {
                x: gap.x + 1,
                width: 1,
                ..gap
            },
        );
    }

    if let Some(workspace) = app.selected() {
        // A cursor exists here exactly while the services column has
        // one, whichever pane the keys happen to be in — because that is
        // exactly when `u` acts on one service rather than all of them.
        let cursor = app.selected_service().map(|service| Cursor {
            service: &service.name,
            active: app.focus() == Focus::Services,
        });

        // The whole reason the views were written the way they were.
        frame.render_widget(
            Framed(&views::workspace(workspace, cursor, Decor::BARE)),
            right,
        );
    }
}

/// The first row to draw, so that the cursor is on screen.
///
/// **A list longer than the pane used to draw its first rows and stop.**
/// The cursor went off the bottom with no mark anywhere on screen, and
/// the next `u` started a workspace nobody could see was selected —
/// which is the case the whole dashboard is for, since running several
/// worktrees at once is the premise.
///
/// The cursor is kept near the middle rather than scrolled by the least
/// that would do: there is no previous offset to scroll from — the
/// drawing holds no state — and a rule that depends on one cannot be a
/// pure function of what is on screen. Clamped at both ends, so the
/// first and last pages are full.
fn window(selected: usize, total: usize, rows: usize) -> usize {
    if rows == 0 || total <= rows {
        return 0;
    }

    selected
        .saturating_sub(rows.saturating_sub(1) / 2)
        .min(total - rows)
}

/// How wide the list may be.
///
/// What it needs, but never more than a third of the window: the pane
/// beside it carries URLs, and a long branch name on the left must not be
/// what cuts one of those in half.
fn list_width(app: &App, available: u16) -> u16 {
    let wanted = app
        .workspaces()
        .iter()
        .map(|workspace| {
            let name = workspace.display_name().width();
            let count = counts(workspace).0.width();
            // The marker, the name, the gap, the count.
            u16::try_from(2 + name + 2 + count).unwrap_or(u16::MAX)
        })
        .max()
        .unwrap_or(MIN_LIST_WIDTH);

    let ceiling = (available / 3).max(MIN_LIST_WIDTH);
    wanted.clamp(MIN_LIST_WIDTH, ceiling).min(available)
}

/// `3/3`, and how it should read.
fn counts(workspace: &kobune_api::WorkspaceInfo) -> (String, ratatui::style::Style) {
    let running = workspace
        .services
        .iter()
        .filter(|service| service.state.is_running())
        .count();
    let total = workspace.services.len();

    let style = if total == 0 || running == 0 {
        theme::secondary()
    } else if running == total {
        theme::good()
    } else {
        theme::warn()
    };

    (format!("{running}/{total}"), style)
}

/// The selectable list.
///
/// Hand-built rather than [`views::workspaces`]: that one is a table of
/// everything, printed once, and has no notion of a row somebody is
/// standing on.
fn list(app: &App, width: u16, height: u16) -> Rows {
    let selected = app.selected_index();
    let count_width = app
        .workspaces()
        .iter()
        .map(|workspace| counts(workspace).0.width())
        .max()
        .unwrap_or(0);

    // The heading takes one of them.
    let rows = usize::from(height).saturating_sub(1);
    let total = app.workspaces().len();
    let first = window(selected.unwrap_or(0), total, rows);

    // Says where in the list this is, and only when there is a rest of
    // the list to be somewhere in.
    let mut heading = vec![Span::styled("workspaces", theme::heading())];
    if rows > 0 && total > rows {
        heading.push(Span::styled(
            format!("  {}/{total}", selected.unwrap_or(0) + 1),
            theme::secondary(),
        ));
    }

    let mut lines = vec![Line::from(heading)];

    // What is left for the name once the marker, the gap and the count
    // have had theirs.
    let room = usize::from(width).saturating_sub(4 + count_width);

    for (index, workspace) in app
        .workspaces()
        .iter()
        .enumerate()
        .skip(first)
        .take(rows.max(1))
    {
        let here = Some(index) == selected;
        let (count, count_style) = counts(workspace);

        // The marker dims when the keys have moved to the other pane, so
        // there is never a question of which cursor `u` is about to act
        // on.
        let marker = match (here, app.focus() == Focus::Workspaces) {
            (true, true) => Span::styled("▸ ", theme::good()),
            (true, false) => Span::styled("▸ ", theme::secondary()),
            (false, _) => Span::raw("  "),
        };

        let name = if here {
            theme::subject()
        } else {
            ratatui::style::Style::new()
        };

        lines.push(Line::from(vec![
            marker,
            Span::styled(pad(&fit(workspace.display_name(), room), room), name),
            Span::raw("  "),
            Span::styled(count, count_style),
        ]));
    }

    Rows(lines)
}

/// How tall the log pane is when it shares the body with the panes.
///
/// Half, so neither is squeezed to nothing — and never less than
/// [`MIN_LOG_HEIGHT`], because a pane showing one line of a log is worth
/// less than the two rows it took to say so.
fn log_pane_height(body: u16) -> u16 {
    (body / 2).max(MIN_LOG_HEIGHT).min(body)
}

/// How many rows of log are on screen, in a window this tall.
///
/// The one fact about the layout the state has to know — a page-up moves
/// by exactly this — so it is worked out here, where the layout is, and
/// handed over rather than guessed at from both ends.
pub(super) fn log_rows(window: u16, full: bool) -> u16 {
    let body = window.saturating_sub(2 + FOOTER_HEIGHT);
    let pane = if full { body } else { log_pane_height(body) };

    // The heading is one of the pane's rows.
    pane.saturating_sub(1)
}

/// The log pane's inner size, in a window this big.
///
/// The state scrolls in rows and wraps to columns, so it has to be told
/// the pane it is scrolling. Worked out here, where the layout is, rather
/// than guessed at from both ends.
pub(super) fn log_viewport(width: u16, height: u16, full: bool) -> (u16, u16) {
    // The border, and the padding inside it.
    (width.saturating_sub(4), log_rows(height, full))
}

/// The log pane: what is being followed, and what it has said.
fn logs(app: &App, frame: &mut Frame, area: Rect) {
    let Some(logs) = app.logs() else {
        return;
    };
    if area.height == 0 {
        return;
    }

    let [heading, body] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);

    frame.render_widget(log_heading(app, logs, heading.width), heading);

    if logs.is_empty() {
        // A pane that draws nothing looks like a pane that is broken.
        frame.render_widget(
            Rows(vec![Line::styled("waiting for output", theme::secondary())]),
            body,
        );
        return;
    }

    // Already wrapped, coloured and columned: the pane scrolls in rows,
    // so the rows are the state's to work out and this only puts them on
    // the screen.
    frame.render_widget(Rows(logs.rows()), body);
}

/// What the pane is following, and whether it still is.
fn log_heading(app: &App, logs: &Logs, width: u16) -> Line<'static> {
    let subject = match &logs.subject.service {
        Some(service) => format!("{} / {}", logs.subject.workspace, service),
        None => logs.subject.workspace.clone(),
    };

    let (status, status_style) = match (&logs.ended, logs.following()) {
        // The stream stopped without being asked to. Saying so beats a
        // pane that has simply gone quiet, which looks the same.
        (Some(ended), _) => (ended.clone(), theme::bad()),
        (None, true) => ("following".to_string(), theme::good()),
        // How much has arrived underneath, so it is clear that the pane
        // is still filling and this is a view of the past.
        (None, false) => (format!("paused · {} below", logs.behind()), theme::warn()),
    };

    let label = if app.focus() == Focus::Logs {
        theme::good()
    } else {
        theme::secondary()
    };

    let used = "logs: ".width() + subject.width() + status.width();
    let gap = usize::from(width).saturating_sub(used).max(1);

    Line::from(vec![
        Span::styled("logs: ", label),
        Span::styled(subject, theme::subject()),
        Span::raw(" ".repeat(gap)),
        Span::styled(status, status_style),
    ])
}

/// What is happening, or what went wrong, on the one line there is for it.
fn activity_line(app: &App) -> Line<'static> {
    if let Some(activity) = app.activity() {
        let spinner = progress::SPINNER[(app.frame() / 2) % progress::SPINNER.len()];

        // The newest step is the one being waited on; the label of the
        // operation itself stands in until the daemon has named one.
        let mut spans = vec![Span::styled(format!("{spinner} "), theme::warn())];

        match activity.steps.last() {
            Some(step) => {
                spans.push(Span::styled(step.label.clone(), theme::subject()));

                if let Some(detail) = &step.detail {
                    spans.push(Span::styled(format!(" · {detail}"), theme::secondary()));
                }

                if activity.steps.len() > 1 {
                    spans.push(Span::styled(
                        format!("  (+{} more)", activity.steps.len() - 1),
                        theme::secondary(),
                    ));
                }
            }
            None => spans.push(Span::styled(activity.label.clone(), theme::subject())),
        }

        return Line::from(spans);
    }

    match app.trouble() {
        // The mark the CLI puts on anything that went wrong, so a reason
        // read here and the same reason read after `kobune up` do not
        // look like two different kinds of thing. It carries the ✗ as
        // well as the colour, for a terminal showing neither.
        Some(trouble) => Line::from(vec![
            Span::styled("✗ ", theme::bad()),
            Span::raw(trouble.to_string()),
        ]),
        None => Line::default(),
    }
}

/// The keys worth naming, for the pane the keys are going to.
///
/// One line, and there are more keys than fit on it. Which ones are worth
/// the room depends on what the cursor is in: reading a log, `↑↓` scrolls
/// and `G` is the way back to the bottom, and neither means anything in
/// the panes above. `?` has the rest either way.
fn key_line(app: &App) -> Line<'static> {
    if app.focus() == Focus::Logs {
        return keys(&[
            ("↑↓", "scroll"),
            ("G", "tail"),
            ("L", "full"),
            ("l", "close"),
            ("tab", "pane"),
            ("?", "keys"),
            ("q", "quit"),
        ]);
    }

    keys(&[
        ("↑↓", "select"),
        ("tab", "pane"),
        ("u", "up"),
        ("d", "down"),
        ("l", "logs"),
        ("o", "open"),
        ("c", "check"),
        ("?", "keys"),
        ("q", "quit"),
    ])
}

fn keys(pairs: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::new();

    for (key, what) in pairs {
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
        }

        spans.push(Span::styled((*key).to_string(), theme::command()));
        spans.push(Span::raw(" "));
        spans.push(Span::styled((*what).to_string(), theme::secondary()));
    }

    Line::from(spans)
}

/// The rows an overlay may take.
///
/// The screen, less the frame it floats inside and its own.
fn overlay_rows(height: u16) -> u16 {
    height.saturating_sub(4).max(1)
}

/// The parts of the layout the state has to know, measured where the
/// layout is.
///
/// Scrolling is why all of these exist: a row is only a row once there
/// is a width to wrap to, a page is only a page once there is a height,
/// and how far a thing scrolls is how much of it there is less how much
/// of it shows. Guessing any of them from the other side is how `G` came
/// to leave the key list unscrollable.
///
/// The overlay is built twice a frame, here and to draw it. These are
/// panels of a dozen lines; measuring them costs less than keeping two
/// answers in step would.
pub(super) fn measure(app: &App, width: u16, height: u16) -> Measured {
    let (log_columns, log_rows) = log_viewport(width, height, app.logs_are_full());

    let overlay_content = app.overlay().map_or(0, |showing| {
        let panel = overlay_panel(showing);
        let inner = panel.preferred_width().min(width.saturating_sub(2)).max(1);
        panel.height(inner).max(1)
    });

    Measured {
        log_columns,
        log_rows,
        overlay_rows: overlay_rows(height),
        overlay_content,
    }
}

/// Whatever is drawn over the dashboard.
///
/// Every one of these is a [`Panel`] the printed commands already built.
/// What is added here is a box to float it in and a window to scroll it
/// through — `kobune doctor` on a machine with something wrong is taller
/// than the screen, and an overlay that lost its bottom would be the
/// thing this pass set out to stop doing.
fn overlay_panel(showing: &Overlay) -> Panel {
    match showing {
        Overlay::Keys => keys_panel(),
        Overlay::Waiting(what) => message(
            "please wait",
            format!("reading {what}…"),
            theme::secondary(),
        ),
        Overlay::Checks(report) => views::diagnostics(report, Decor::FRAMED),
        Overlay::Env { entries, service } => views::env(entries, service.as_deref(), Decor::FRAMED),
        Overlay::Code(service) => views::url(service, Decor::FRAMED),
        Overlay::Failed { what, reason } => message(
            "nothing to show",
            format!("could not read {what}: {reason}"),
            theme::bad(),
        ),
    }
}

fn overlay(app: &App, showing: &Overlay, frame: &mut Frame) {
    let panel = overlay_panel(showing);

    let screen = frame.area();
    if screen.width < 8 || screen.height < 4 {
        return;
    }

    let width = panel
        .preferred_width()
        .min(screen.width.saturating_sub(2))
        .max(1);
    let full = panel.height(width).max(1);
    let height = full.min(overlay_rows(screen.height));

    let area = centred(screen, width, height);
    let offset = app.overlay_scroll().min(full.saturating_sub(height));

    // Nothing of what is behind shows through the gaps.
    frame.render_widget(Clear, area);
    frame.render_widget(
        Window {
            view: &panel,
            offset,
        },
        area,
    );

    // There is more of it than fits, and which way says which key.
    if offset > 0 {
        mark(frame, area, area.y, "↑");
    }
    if offset + height < full {
        mark(frame, area, area.bottom().saturating_sub(1), "↓");
    }
}

/// An arrow on the box's edge, where a scrollbar would be.
fn mark(frame: &mut Frame, area: Rect, y: u16, arrow: &'static str) {
    let Some(x) = area.right().checked_sub(2) else {
        return;
    };

    frame.render_widget(Line::styled(arrow, theme::warn()), Rect::new(x, y, 1, 1));
}

/// A box with one thing to say in it.
fn message(title: &'static str, text: String, style: ratatui::style::Style) -> Panel {
    Panel::new(Decor::FRAMED, title).line(Span::styled(text, style))
}

/// The key list, as a panel like every other overlay.
fn keys_panel() -> Panel {
    let rows: [(&str, &str); 15] = [
        ("↑ ↓ / j k", "move the cursor, or scroll"),
        ("tab / ← →", "switch between the panes"),
        ("pg up / pg dn", "a screenful at a time"),
        ("g / G", "the top, and the bottom"),
        ("u", "start — one service, or the whole workspace"),
        ("d", "stop, the same way"),
        ("o", "open the URL in a browser"),
        ("Q", "the URL as a code to photograph"),
        ("l", "follow the logs of what the cursor is on"),
        ("L", "give the logs the whole screen"),
        ("c", "check this machine over, as `kobune doctor` does"),
        ("e", "the environment variables, masked"),
        ("r", "re-fetch now"),
        ("?", "this list"),
        ("q", "leave — esc backs out one step at a time"),
    ];

    let mut grid = Grid::new();
    for (key, what) in rows {
        grid.push(vec![
            Line::styled(key, theme::command()),
            Line::styled(what, theme::secondary()),
        ]);
    }

    Panel::new(Decor::FRAMED, "keys").grid(grid)
}

/// A window onto a view taller than the room it is given.
///
/// [`ratatui::Frame`] hands out no buffer, and a view cannot be drawn at
/// a negative offset, so it is drawn once into a buffer of its own full
/// height and the rows wanted are copied across.
struct Window<'a, V: View> {
    view: &'a V,
    offset: u16,
}

impl<V: View> Widget for Window<'_, V> {
    fn render(self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let height = self.view.height(area.width).max(1);
        let mut scratch = ratatui::buffer::Buffer::empty(Rect::new(0, 0, area.width, height));
        self.view.render(scratch.area, &mut scratch);

        for row in 0..area.height {
            let from = self.offset.saturating_add(row);
            if from >= height {
                return;
            }

            for column in 0..area.width {
                if let Some(cell) = scratch.cell((column, from)).cloned()
                    && let Some(target) = buf.cell_mut((area.x + column, area.y + row))
                {
                    *target = cell;
                }
            }
        }
    }
}

/// A box of that size in the middle, or as much of one as there is room
/// for.
fn centred(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);

    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// Lines, one per row, clipped to what they are given.
struct Rows(Vec<Line<'static>>);

impl Widget for Rows {
    fn render(self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        for (index, line) in self.0.into_iter().enumerate() {
            let Ok(offset) = u16::try_from(index) else {
                return;
            };
            let Some(y) = area.y.checked_add(offset) else {
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
mod tests {
    use super::*;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    use kobune_api::{ServiceInfo, ServiceScope, WorkspaceInfo};
    use kobune_core::ServiceState;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::path::{Path, PathBuf};

    fn service(name: &str, state: ServiceState) -> ServiceInfo {
        ServiceInfo {
            name: name.into(),
            state,
            reason: None,
            scope: ServiceScope::Workspace,
            url: Some(format!("https://{name}.feat-1.myapp.localhost")),
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
                    service("db", ServiceState::Stopped),
                ],
            ),
            workspace("feat-2", vec![service("web", ServiceState::Stopped)]),
        ]
    }

    /// What would reach a terminal that cannot show colour, which is what
    /// the assertions are about.
    /// Measures the layout and then draws it, which is the order the
    /// loop does it in — the scroll bounds come from the measurement.
    fn screen(app: &mut App, width: u16, height: u16) -> String {
        app.resize(measure(app, width, height));

        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("opens");
        terminal.draw(|frame| draw(app, frame)).expect("draws");

        crate::ui::surface::render_to_string(terminal.backend().buffer(), false)
    }

    #[test]
    fn both_panes_are_there() {
        let text = screen(
            &mut App::new(listing(), Path::new("/repo.wt/feat-1"), None),
            90,
            20,
        );

        assert!(text.contains("kobune ─ myapp"), "got:\n{text}");
        assert!(text.contains("feat-1"), "got:\n{text}");
        assert!(text.contains("feat-2"), "got:\n{text}");
        // The right-hand pane is `views::workspace`, unchanged.
        assert!(text.contains("web"), "got:\n{text}");
        assert!(text.contains("ready"), "got:\n{text}");
        assert!(
            text.contains("https://web.feat-1.myapp.localhost"),
            "got:\n{text}"
        );
    }

    #[test]
    fn the_cursor_is_visible() {
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        assert!(
            screen(&mut app, 90, 20).contains("▸ feat-1"),
            "on the first"
        );

        app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let text = screen(&mut app, 90, 20);
        assert!(text.contains("▸ feat-2"), "got:\n{text}");
        assert_eq!(text.matches('▸').count(), 1, "one cursor only:\n{text}");
    }

    #[test]
    fn a_list_longer_than_the_pane_keeps_its_cursor_on_screen() {
        // It used to draw the first rows and stop: the cursor went off
        // the bottom with no mark anywhere, and the next `u` started a
        // workspace nobody could see was selected.
        let many: Vec<WorkspaceInfo> = (0..30)
            .map(|n| {
                workspace(
                    &format!("feat-{n}"),
                    vec![service("web", ServiceState::Ready)],
                )
            })
            .collect();

        let mut app = App::new(many, Path::new("/nowhere"), None);
        for _ in 0..29 {
            app.on_key(press(KeyCode::Down));
        }

        let text = screen(&mut app, 90, 24);
        assert_eq!(app.selected().expect("selected").display_name(), "feat-29");
        assert!(text.contains("▸ feat-29"), "the cursor is drawn:\n{text}");
        assert!(
            text.contains("30/30"),
            "and says where in the list:\n{text}"
        );
    }

    #[test]
    fn a_list_that_fits_says_nothing_about_where_it_is() {
        // The count is for finding your way through a list too long to
        // see, and there is no way to lose in a list of two.
        let text = screen(
            &mut App::new(listing(), Path::new("/repo.wt/feat-1"), None),
            90,
            24,
        );

        let heading = text
            .lines()
            .find(|line| line.contains("workspaces"))
            .expect("the heading is drawn");

        // The row carries the right-hand pane too; the count would sit
        // immediately after the word.
        assert!(
            heading.contains("workspaces  "),
            "no count after it: {heading:?}"
        );
        assert!(
            !heading.contains("workspaces  1/"),
            "a list of two cannot be lost in: {heading:?}"
        );
    }

    #[test]
    fn the_right_pane_follows_the_cursor() {
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));

        let text = screen(&mut app, 90, 20);
        assert!(text.contains("feature/feat-2"), "got:\n{text}");
        assert!(!text.contains("feature/feat-1"), "got:\n{text}");
    }

    #[test]
    fn the_keys_are_always_on_screen() {
        let text = screen(
            &mut App::new(listing(), Path::new("/repo.wt/feat-1"), None),
            90,
            20,
        );
        assert!(text.contains("quit"), "got:\n{text}");
        assert!(text.contains("logs"), "got:\n{text}");
        assert!(text.contains("keys"), "got:\n{text}");
    }

    #[test]
    fn the_key_line_names_the_keys_that_do_something_here() {
        // There are more keys than fit on one line, and which ones are
        // worth the room depends on the pane: `G` means nothing in a
        // list of workspaces, and `u` means nothing while reading a log.
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        assert!(screen(&mut app, 90, 20).contains(" up"));

        app.on_key(press(KeyCode::Char('l')));
        let text = screen(&mut app, 90, 20);

        assert!(text.contains("scroll"), "got:\n{text}");
        assert!(text.contains("tail"), "got:\n{text}");
    }

    #[test]
    fn an_operation_says_what_it_is_doing() {
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));

        let text = screen(&mut app, 90, 20);
        assert!(text.contains("starting feat-1"), "got:\n{text}");

        app.on_event(&kobune_api::Event::Step {
            id: "pull".into(),
            label: "pulling node:22-alpine".into(),
            status: kobune_api::StepStatus::Started,
        });

        let text = screen(&mut app, 90, 20);
        assert!(text.contains("pulling node:22-alpine"), "got:\n{text}");
    }

    #[test]
    fn trouble_reads_as_trouble() {
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.went_wrong("cannot reach the daemon".into());

        let text = screen(&mut app, 90, 20);
        assert!(text.contains("✗ cannot reach the daemon"), "got:\n{text}");
    }

    #[test]
    fn the_help_overlay_covers_what_is_behind_it() {
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));

        let text = screen(&mut app, 90, 20);
        assert!(text.contains("keys"), "got:\n{text}");
        assert!(text.contains("switch between the panes"), "got:\n{text}");
    }

    #[test]
    fn an_overlay_taller_than_the_screen_scrolls_rather_than_losing_its_bottom() {
        // The whole point of the box being a window onto the view rather
        // than the view itself.
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Char('?')));

        // Ten rows of screen, fifteen keys to list.
        let text = screen(&mut app, 90, 10);
        assert!(text.contains("↓"), "it says there is more: \n{text}");
        assert!(!text.contains("leave"), "the last row is not on yet");

        for _ in 0..20 {
            app.on_key(press(KeyCode::Down));
        }
        let text = screen(&mut app, 90, 10);

        assert!(text.contains("leave"), "scrolled to it: \n{text}");
        assert!(text.contains("↑"), "and says where it came from");
    }

    #[test]
    fn the_key_list_is_wide_enough_for_what_is_in_it() {
        // The box used to be measured row by row, and the widest
        // description does not share a row with the widest key — so the
        // longest line in the list was the one nobody could read.
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Char('?')));

        let text = screen(&mut app, 100, 24);
        assert!(
            text.contains("start — one service, or the whole workspace"),
            "cut off:\n{text}"
        );
        assert!(
            text.contains("give the logs the whole screen"),
            "cut off:\n{text}"
        );
    }

    #[test]
    fn an_empty_listing_says_what_to_do_about_it() {
        let text = screen(
            &mut App::new(Vec::new(), Path::new("/repo.wt/feat-1"), None),
            90,
            20,
        );

        assert!(text.contains("none yet"), "got:\n{text}");
        assert!(text.contains("kobune new <branch>"), "got:\n{text}");
    }

    #[test]
    fn the_log_pane_shows_what_it_is_following_and_what_it_said() {
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Char('l')));
        app.on_log(Some("web".into()), "ready on :3000".into());

        let text = screen(&mut app, 90, 20);
        assert!(text.contains("logs: feat-1"), "got:\n{text}");
        assert!(text.contains("following"), "got:\n{text}");
        assert!(text.contains("ready on :3000"), "got:\n{text}");
        // Still a dashboard: the panes above it are there.
        assert!(text.contains("workspaces"), "got:\n{text}");
    }

    #[test]
    fn a_full_screen_of_logs_has_nothing_else_on_it() {
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Char('L')));
        app.on_log(Some("web".into()), "ready on :3000".into());

        let text = screen(&mut app, 90, 20);
        assert!(text.contains("ready on :3000"), "got:\n{text}");
        assert!(!text.contains("workspaces"), "got:\n{text}");
    }

    #[test]
    fn a_programs_own_colour_does_not_reach_the_buffer_as_text() {
        // Every line of a real dev server's output looks like this.
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Char('l')));
        app.on_log(
            Some("api".into()),
            "\u{1b}[32m[info]\u{1b}[39m GET /ok \u{1b}[1m200\u{1b}[22m".into(),
        );

        let text = screen(&mut app, 90, 20);
        assert!(text.contains("[info] GET /ok 200"), "got:\n{text}");
        assert!(!text.contains('\u{1b}'), "got:\n{text}");
        assert!(!text.contains("[32m"), "got:\n{text}");
    }

    #[test]
    fn scrolling_up_says_how_much_is_underneath() {
        // A view of the past that looked like a view of the present
        // would have somebody waiting for a line that already arrived.
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Char('l')));

        for n in 0..40 {
            app.on_log(None, format!("line {n}"));
        }
        app.on_key(press(KeyCode::Up));
        app.on_key(press(KeyCode::Up));

        let text = screen(&mut app, 90, 20);
        assert!(text.contains("paused"), "got:\n{text}");
        assert!(text.contains("2 below"), "got:\n{text}");
        assert!(!text.contains("following"), "got:\n{text}");
    }

    #[test]
    fn the_service_column_is_only_there_when_it_says_something() {
        // Following one service, its name down the side of its own
        // output is the same word on every row.
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Tab));
        app.on_key(press(KeyCode::Char('l')));
        app.on_log(Some("web".into()), "only mine".into());

        let text = screen(&mut app, 90, 20);
        assert!(text.contains("logs: feat-1 / web"), "got:\n{text}");

        let row = text
            .lines()
            .find(|line| line.contains("only mine"))
            .expect("the line is drawn");

        assert!(!row.contains("web"), "no column of its own name: {row:?}");
    }

    #[test]
    fn an_empty_pane_says_it_is_waiting_rather_than_showing_nothing() {
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        app.on_key(press(KeyCode::Char('l')));

        assert!(screen(&mut app, 90, 20).contains("waiting for output"));
    }

    #[test]
    fn the_services_cursor_is_visible_in_the_pane_it_acts_on() {
        // Without this, tab moves the cursor somewhere invisible and `u`
        // acts on a service nobody can see is selected.
        let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
        assert_eq!(
            screen(&mut app, 90, 20).matches('▸').count(),
            1,
            "only the workspace cursor to begin with"
        );

        app.on_key(press(KeyCode::Tab));
        let text = screen(&mut app, 90, 20);

        assert_eq!(
            text.matches('▸').count(),
            2,
            "the services cursor joins it:\n{text}"
        );
        // The gap is the grid's own column spacing.
        assert!(
            text.contains("▸  ● web"),
            "against the row it is on:\n{text}"
        );
    }

    #[test]
    fn a_window_too_small_to_draw_in_is_not_a_panic() {
        // Somebody drags the corner. Every one of these has been a
        // subtraction overflow in a layout at some point.
        for (width, height) in [(80, 24), (40, 10), (20, 6), (10, 3), (4, 2), (1, 1)] {
            let mut app = App::new(listing(), Path::new("/repo.wt/feat-1"), None);
            screen(&mut app, width, height);

            // And with the pane that takes its rows from everything else.
            app.on_key(press(KeyCode::Char('l')));
            app.on_log(Some("web".into()), "a line".into());
            screen(&mut app, width, height);

            app.on_key(press(KeyCode::Char('L')));
            screen(&mut app, width, height);
        }
    }

    #[test]
    fn a_long_name_is_cut_rather_than_pushing_the_count_off() {
        let long = workspace(
            "a-very-long-branch-name-indeed-truly",
            vec![service("web", ServiceState::Ready)],
        );

        let text = screen(
            &mut App::new(vec![long], Path::new("/repo.wt/feat-1"), None),
            60,
            12,
        );
        assert!(text.contains('…'), "the name is cut:\n{text}");
        assert!(text.contains("1/1"), "the count survives it:\n{text}");
    }

    #[test]
    fn fitting_counts_columns_rather_than_characters() {
        // A CJK branch name is two columns a glyph, and cutting by
        // `chars()` would overrun the column beside it.
        assert_eq!(fit("web", 10), "web");
        assert_eq!(fit("abcdef", 4), "abc…");
        assert!(fit("日本語のブランチ", 6).width() <= 6);
    }
}
