//! Getting a drawn buffer out of the process.
//!
//! ratatui draws into a [`Buffer`] and a backend then paints it by moving
//! the cursor around the screen. A command that prints once and exits
//! wants none of that: the lines have to land in the scrollback, in order,
//! so that `minato status | grep web` keeps working and so that scrolling
//! up tomorrow still shows what happened.
//!
//! So the buffer is walked here and written as ordinary lines. The widgets
//! above are unchanged by it — the same views can be handed to a real
//! [`ratatui::Terminal`] the day `minato` grows a full-screen mode.

use std::io::{IsTerminal, Write};

use ratatui::backend::IntoCrossterm;
use ratatui::buffer::{Buffer, Cell};
use ratatui::crossterm::style::ContentStyle;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use unicode_width::UnicodeWidthStr as _;

use super::View;
use super::theme::Decor;

/// Narrower than this and a table is unreadable however small the window.
const MIN_WIDTH: u16 = 32;

/// Where a view is going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// A destination, and what it can be sent.
pub struct Surface {
    stream: Stream,
    /// Whether something is watching that can be drawn on, as opposed to a
    /// pipe, a log file or a terminal that shows escape sequences as text.
    interactive: bool,
    /// Whether ANSI styling reaches a person rather than a log file.
    styled: bool,
    decor: Decor,
    /// The widest a view may draw itself.
    limit: u16,
}

impl Surface {
    pub fn stdout() -> Self {
        Self::for_stream(Stream::Stdout)
    }

    pub fn stderr() -> Self {
        Self::for_stream(Stream::Stderr)
    }

    pub fn for_stream(stream: Stream) -> Self {
        let is_terminal = match stream {
            Stream::Stdout => std::io::stdout().is_terminal(),
            Stream::Stderr => std::io::stderr().is_terminal(),
        };

        // A terminal that cannot draw is treated as a pipe throughout:
        // `TERM=dumb` is how emacs' shell and a handful of CI runners
        // announce that escape sequences arrive as literal text.
        let interactive = is_terminal && !is_dumb();
        let styled = interactive && !no_color();

        // Framed and coloured are not the same question: `NO_COLOR` on a
        // terminal keeps the frame and drops the colour, and a view that
        // draws something colour has to carry has to be told which.
        let decor = if interactive {
            Decor::FRAMED
        } else {
            Decor::PLAIN
        };

        Self {
            stream,
            interactive,
            styled,
            decor: if styled { decor } else { decor.unstyled() },
            // A pipe has no width to run out of, so a view is given
            // whatever it asks for: nothing that goes into a log, a
            // `grep` or an agent's transcript is ever wrapped or cut.
            limit: if interactive {
                terminal_width()
            } else {
                u16::MAX
            },
        }
    }

    /// Whether a live display is worth attempting.
    pub fn is_interactive(&self) -> bool {
        self.interactive
    }

    /// Draws a view and writes it out.
    ///
    /// The view is built from the decoration rather than handed it
    /// afterwards, so no view can forget to ask.
    pub fn print<V: View>(&self, build: impl FnOnce(Decor) -> V) {
        let view = build(self.decor);

        // A view that fits gets exactly the width it asked for, which is
        // what keeps a frame drawn around three services three services
        // wide instead of stretched across the window.
        let width = view
            .preferred_width()
            .clamp(MIN_WIDTH, self.limit.max(MIN_WIDTH));
        let height = view.height(width);
        if height == 0 {
            return;
        }

        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        view.render(area, &mut buffer);

        self.write(&render_to_string(&buffer, self.styled));
    }

    /// Writes text through, for the one-line answers (`url`, `env get`)
    /// that exist to be piped and must stay undecorated.
    pub fn line(&self, text: &str) {
        self.write(&format!("{text}\n"));
    }

    fn write(&self, text: &str) {
        // A closed pipe kills the process through SIGPIPE, which main
        // restores on purpose, so there is nothing left to report here.
        let _ = match self.stream {
            Stream::Stdout => {
                let mut out = std::io::stdout().lock();
                out.write_all(text.as_bytes()).and_then(|()| out.flush())
            }
            Stream::Stderr => {
                let mut out = std::io::stderr().lock();
                out.write_all(text.as_bytes()).and_then(|()| out.flush())
            }
        };
    }
}

/// Turns a drawn buffer into the text to write.
///
/// Cells sharing a style are written as one run, and trailing blanks are
/// dropped: a line of padding out to the buffer's width would break `git
/// diff --check` on a captured log, and it is invisible either way.
///
/// A blank cell with a background is *not* invisible, so it stays — that is
/// the right-hand quiet zone of a QR code, and a code with three sides of
/// margin is one a scanner has to guess the edge of. Only where the
/// background will actually be written, though: with styling off it is a
/// space like any other, and keeping it would be the trailing whitespace
/// this trim exists to remove.
pub(super) fn render_to_string(buffer: &Buffer, styled: bool) -> String {
    let mut out = String::new();

    for y in buffer.area.top()..buffer.area.bottom() {
        let mut cells: Vec<&Cell> = Vec::new();

        // A wide glyph — CJK, an emoji — occupies two cells and lives in
        // the first. The cells it covers are skipped rather than written,
        // or `日本語` comes out as `日 本 語`.
        let mut covered = 0usize;
        for x in buffer.area.left()..buffer.area.right() {
            let Some(cell) = buffer.cell((x, y)) else {
                continue;
            };

            if covered > 0 {
                covered -= 1;
                continue;
            }

            covered = cell.symbol().width().saturating_sub(1);
            cells.push(cell);
        }

        while cells.last().is_some_and(|cell| {
            cell.symbol().trim().is_empty() && (!styled || cell.bg == Color::Reset)
        }) {
            cells.pop();
        }

        let mut run = String::new();
        let mut run_style: Option<Style> = None;

        for cell in cells {
            let style = style_of(cell);

            if run_style != Some(style) {
                flush_run(&mut out, &run, run_style, styled);
                run.clear();
                run_style = Some(style);
            }

            run.push_str(cell.symbol());
        }

        flush_run(&mut out, &run, run_style, styled);
        out.push('\n');
    }

    out
}

fn flush_run(out: &mut String, run: &str, style: Option<Style>, styled: bool) {
    if run.is_empty() {
        return;
    }

    match style.filter(|_| styled) {
        Some(style) => {
            let content: ContentStyle = style.into_crossterm();
            out.push_str(&content.apply(run).to_string());
        }
        None => out.push_str(run),
    }
}

/// The style to write a cell with.
///
/// `Color::Reset` is left off rather than written out. It would be
/// harmless — it is what the terminal is already showing — but it is an
/// escape sequence per run against text that mostly has no colour at all.
fn style_of(cell: &Cell) -> Style {
    let mut style = Style::new().add_modifier(cell.modifier);

    if cell.fg != Color::Reset {
        style = style.fg(cell.fg);
    }
    if cell.bg != Color::Reset {
        style = style.bg(cell.bg);
    }

    style
}

/// How wide the window is, or the conventional guess.
///
/// A pseudo-terminal that nobody sized — `script`, some CI runners, a
/// process reparented after its parent exited — reports zero columns, and
/// taking that literally squeezes every table down to the minimum.
fn terminal_width() -> u16 {
    match ratatui::crossterm::terminal::size() {
        Ok((columns, _)) if columns >= MIN_WIDTH => columns,
        _ => 80,
    }
}

/// The window's size, where the terminal knows it.
///
/// `None` in the cases [`terminal_width`] guesses its way out of. This
/// answer is not a layout to fall back on: it is handed to a program that
/// will draw to exactly what it is told, and a terminal reported as 0×0
/// would have it draw to nothing.
pub fn window() -> Option<minato_api::Window> {
    match ratatui::crossterm::terminal::size() {
        Ok((columns, rows)) if columns >= MIN_WIDTH && rows > 0 => {
            Some(minato_api::Window::new(columns, rows))
        }
        _ => None,
    }
}

fn no_color() -> bool {
    // https://no-color.org: set to anything at all, and nothing coloured.
    std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty())
}

fn is_dumb() -> bool {
    std::env::var("TERM").is_ok_and(|term| term == "dumb")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Stylize};
    use ratatui::text::Line;
    use ratatui::widgets::Widget;

    fn draw(width: u16, height: u16, lines: &[Line<'_>]) -> Buffer {
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        for (row, line) in lines.iter().enumerate() {
            let row = Rect::new(0, row as u16, width, 1);
            line.clone().render(row, &mut buffer);
        }
        buffer
    }

    #[test]
    fn drops_the_padding_a_buffer_comes_with() {
        // The buffer is 40 wide; "web" is not.
        let buffer = draw(40, 1, &[Line::raw("web")]);
        assert_eq!(render_to_string(&buffer, false), "web\n");
    }

    #[test]
    fn a_blank_that_paints_is_not_padding() {
        // A QR code's right-hand quiet zone is blank cells with a white
        // ground. Trimmed away with the padding, the code loses the margin
        // on one side and a scanner has no edge to find there.
        let buffer = draw(8, 1, &[Line::from(vec!["x".into(), "  ".bg(Color::White)])]);
        let text = render_to_string(&buffer, true);

        assert!(text.contains("  "), "the painted blanks stay: {text:?}");
    }

    #[test]
    fn a_background_nobody_will_write_is_padding_after_all() {
        // Unstyled, that quiet zone reaches the file as two spaces at the
        // end of a line — which is what this trim is for. `minato url --qr
        // | tee log` should not be the one thing that leaves them behind.
        let buffer = draw(8, 1, &[Line::from(vec!["x".into(), "  ".bg(Color::White)])]);
        assert_eq!(render_to_string(&buffer, false), "x\n");
    }

    #[test]
    fn writes_one_line_per_row() {
        let buffer = draw(10, 2, &[Line::raw("one"), Line::raw("two")]);
        assert_eq!(render_to_string(&buffer, false), "one\ntwo\n");
    }

    #[test]
    fn leaves_no_escape_sequence_behind_when_unstyled() {
        // This is what reaches a pipe, a CI log and an agent's transcript.
        let buffer = draw(20, 1, &[Line::from("web".green().bold())]);
        let text = render_to_string(&buffer, false);

        assert_eq!(text, "web\n");
        assert!(!text.contains('\u{1b}'), "got: {text:?}");
    }

    #[test]
    fn colours_reach_a_terminal() {
        let buffer = draw(20, 1, &[Line::from("web".green())]);
        let text = render_to_string(&buffer, true);

        assert!(text.contains('\u{1b}'), "got: {text:?}");
        assert!(text.contains("web"), "got: {text:?}");
    }

    #[test]
    fn a_run_of_one_style_is_written_once() {
        // Per-cell escape sequences would work and would also make the
        // output several times its size.
        let buffer = draw(20, 1, &[Line::from("ready".fg(Color::Green))]);
        let text = render_to_string(&buffer, true);

        assert_eq!(text.matches("ready").count(), 1);
        // One sequence to set the colour, one to put it back — not one
        // per character.
        assert!(text.matches('\u{1b}').count() <= 3, "got: {text:?}");
    }

    #[test]
    fn plain_text_carries_no_escape_sequences_at_all() {
        // Most of what these views print has no colour on it. Writing a
        // reset around every run of it would be pure noise on the wire.
        let buffer = draw(20, 1, &[Line::raw("web")]);
        assert_eq!(render_to_string(&buffer, true), "web\n");
    }

    #[test]
    fn styling_stops_at_the_end_of_the_run() {
        // A style left hanging bleeds into the shell prompt.
        let buffer = draw(20, 1, &[Line::from(vec!["a".red(), "b".into()])]);
        let text = render_to_string(&buffer, true);

        assert!(text.ends_with("b\n"), "got: {text:?}");
    }

    #[test]
    fn keeps_wide_glyphs_whole() {
        // A CJK path or branch name occupies two cells and carries its
        // symbol in the first; the second must not become a space.
        let buffer = draw(20, 1, &[Line::raw("日本語")]);
        assert_eq!(render_to_string(&buffer, false), "日本語\n");
    }
}
