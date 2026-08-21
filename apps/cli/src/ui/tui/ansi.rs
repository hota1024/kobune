//! What a container wrote, made drawable.
//!
//! `Event::Output` carries the line exactly as the program produced it,
//! which is the right thing for `kobune logs` — it goes straight to a
//! terminal that understands it. A pane inside a drawn screen is not that
//! terminal: escape sequences reaching a [`ratatui::buffer::Buffer`] are
//! written as text, and a wrangler line arrives as
//! `^[[36m@app/api:dev: ^[[0m^[[32m[wrangler:info]^[[39m` — every line of
//! it, in real output.
//!
//! So the sequences are read here instead. **Colour is kept**, because it
//! is most of what makes a log skimmable: the service that wrote the
//! line, the status code, the one word in red. Everything else a terminal
//! would act on — cursor movement, screen clearing, the window title — is
//! dropped, since a pane has no cursor to move and the line is already a
//! line.
//!
//! Written here rather than pulled in: it is one escape sequence out of
//! the several dozen a terminal implements, and the CLI has no other use
//! for the rest.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar as _;

/// Where a tab lands. The width a terminal uses, so output aligned for
/// one still lines up here.
const TAB: usize = 8;

/// One line of a program's output, as spans to draw.
pub fn line(text: &str) -> Line<'static> {
    let text = last_overwrite(text);

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut style = Style::default();
    let mut column = 0usize;

    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '\u{1b}' => match chars.peek() {
                Some('[') => {
                    chars.next();
                    let (params, ended) = read_csi(&mut chars);

                    // `m` is the only one that says anything about how
                    // the text looks. The rest move a cursor this pane
                    // does not have.
                    if ended == Some('m') {
                        flush(&mut spans, &mut run, style);
                        style = sgr(style, &params);
                    }
                }
                // The window title and friends, which run until a
                // terminator rather than a letter.
                Some(']') => {
                    chars.next();
                    skip_osc(&mut chars);
                }
                // A two-character escape. Nothing it can say matters
                // here, and dropping the letter with it stops it being
                // written out as text.
                Some(_) => {
                    chars.next();
                }
                None => {}
            },

            '\t' => {
                let width = TAB - (column % TAB);
                run.push_str(&" ".repeat(width));
                column += width;
            }

            // A bell, a stray backspace, a vertical tab. A terminal does
            // something with these; a row of a buffer cannot.
            ch if (ch as u32) < 0x20 || ch == '\u{7f}' => {}

            ch => {
                run.push(ch);
                column += ch.width().unwrap_or(0);
            }
        }
    }

    flush(&mut spans, &mut run, style);
    Line::from(spans)
}

/// What is left after the carriage returns have had their way.
///
/// A progress bar redraws itself by returning to column 0 and writing
/// over what is there, so the line as it was last seen is the part after
/// the final `\r`. Kept whole when the `\r` is trailing, which is what a
/// program that ends its line that way meant.
fn last_overwrite(text: &str) -> &str {
    if !text.contains('\r') {
        return text;
    }

    text.rsplit('\r')
        .find(|part| !part.is_empty())
        .unwrap_or_default()
}

/// Reads the rest of a CSI sequence: its parameters, and the letter that
/// says what it was.
///
/// `None` for a sequence the line ended in the middle of, which is then
/// dropped along with its parameters rather than printed.
fn read_csi(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> (String, Option<char>) {
    let mut params = String::new();

    for ch in chars {
        // The final byte of a CSI sequence is anything in this range;
        // everything before it is parameters and intermediates.
        if ('\u{40}'..='\u{7e}').contains(&ch) {
            return (params, Some(ch));
        }

        params.push(ch);
    }

    (params, None)
}

/// Skips an OSC sequence, which ends at a bell or a string terminator
/// rather than at a letter.
fn skip_osc(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while let Some(ch) = chars.next() {
        match ch {
            '\u{7}' => return,
            '\u{1b}' => {
                // `ESC \` is the other terminator. Anything else after
                // the escape belongs to whatever comes next.
                if chars.peek() == Some(&'\\') {
                    chars.next();
                }
                return;
            }
            _ => {}
        }
    }
}

/// Applies one `ESC [ … m` to the style in force.
fn sgr(mut style: Style, params: &str) -> Style {
    // `ESC[m` is `ESC[0m`.
    if params.is_empty() {
        return Style::default();
    }

    let codes: Vec<u16> = params
        .split(';')
        // An empty parameter is a zero: `ESC[;31m` sets red on a reset.
        .map(|code| code.trim().parse::<u16>().unwrap_or(0))
        .collect();

    let mut index = 0;
    while let Some(&code) = codes.get(index) {
        index += 1;

        match code {
            0 => style = Style::default(),
            1 => style.add_modifier |= Modifier::BOLD,
            2 => style.add_modifier |= Modifier::DIM,
            3 => style.add_modifier |= Modifier::ITALIC,
            4 => style.add_modifier |= Modifier::UNDERLINED,
            7 => style.add_modifier |= Modifier::REVERSED,
            9 => style.add_modifier |= Modifier::CROSSED_OUT,
            // One code puts back both of the two it can turn on.
            22 => style.add_modifier.remove(Modifier::BOLD | Modifier::DIM),
            23 => style.add_modifier.remove(Modifier::ITALIC),
            24 => style.add_modifier.remove(Modifier::UNDERLINED),
            27 => style.add_modifier.remove(Modifier::REVERSED),
            29 => style.add_modifier.remove(Modifier::CROSSED_OUT),

            30..=37 => style.fg = Some(basic(code - 30)),
            90..=97 => style.fg = Some(bright(code - 90)),
            39 => style.fg = Some(Color::Reset),

            40..=47 => style.bg = Some(basic(code - 40)),
            100..=107 => style.bg = Some(bright(code - 100)),
            49 => style.bg = Some(Color::Reset),

            // `38;5;n` and `38;2;r;g;b`, and the same for the background.
            // A program that asks for one of these has picked a colour
            // deliberately, so it is honoured rather than rounded to the
            // sixteen — this is the program's palette, not Kobune's.
            38 | 48 => {
                let Some(colour) = extended(&codes, &mut index) else {
                    // Malformed, and the codes after it belong to a
                    // sequence that cannot be read. Stopping beats
                    // reading the red of an `rgb` as a colour of its own.
                    break;
                };

                if code == 38 {
                    style.fg = Some(colour);
                } else {
                    style.bg = Some(colour);
                }
            }

            _ => {}
        }
    }

    style
}

/// The colour named by `5;n` or `2;r;g;b`, moving `index` past it.
fn extended(codes: &[u16], index: &mut usize) -> Option<Color> {
    let kind = *codes.get(*index)?;
    *index += 1;

    match kind {
        5 => {
            let n = *codes.get(*index)?;
            *index += 1;
            Some(Color::Indexed(u8::try_from(n).ok()?))
        }
        2 => {
            let red = u8::try_from(*codes.get(*index)?).ok()?;
            let green = u8::try_from(*codes.get(*index + 1)?).ok()?;
            let blue = u8::try_from(*codes.get(*index + 2)?).ok()?;
            *index += 3;
            Some(Color::Rgb(red, green, blue))
        }
        _ => None,
    }
}

fn basic(offset: u16) -> Color {
    match offset {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        _ => Color::Gray,
    }
}

fn bright(offset: u16) -> Color {
    match offset {
        0 => Color::DarkGray,
        1 => Color::LightRed,
        2 => Color::LightGreen,
        3 => Color::LightYellow,
        4 => Color::LightBlue,
        5 => Color::LightMagenta,
        6 => Color::LightCyan,
        _ => Color::White,
    }
}

fn flush(spans: &mut Vec<Span<'static>>, run: &mut String, style: Style) {
    if run.is_empty() {
        return;
    }

    spans.push(Span::styled(std::mem::take(run), style));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The text alone, which is what a terminal without colour shows.
    fn text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn a_real_wrangler_line_comes_out_readable() {
        // Copied from `kobune logs` against a running project. Every line
        // of that output looks like this.
        let raw = "\u{1b}[36m@app/api:dev: \u{1b}[0m\u{1b}[32m[wrangler:info]\u{1b}[39m \
                   \u{1b}[0m\u{1b}[1mGET\u{1b}[22m /api/auth/ok \u{1b}[32m\u{1b}[1m200\u{1b}[22m \
                   OK \u{1b}[39m\u{1b}[90m(8ms)\u{1b}[39m\u{1b}[0m";

        let line = line(raw);
        assert_eq!(
            text(&line),
            "@app/api:dev: [wrangler:info] GET /api/auth/ok 200 OK (8ms)"
        );
        assert!(
            !text(&line).contains('\u{1b}'),
            "no escape reaches the buffer"
        );
    }

    #[test]
    fn colour_survives() {
        let line = line("\u{1b}[31mfailed\u{1b}[0m ok");

        assert_eq!(line.spans[0].content, "failed");
        assert_eq!(line.spans[0].style.fg, Some(Color::Red));
        assert_eq!(line.spans[1].content, " ok");
        assert_eq!(line.spans[1].style.fg, None);
    }

    #[test]
    fn bold_is_turned_off_by_the_code_that_turns_it_off() {
        // `22` puts back both intensities, and wrangler leans on it.
        let line = line("\u{1b}[1mGET\u{1b}[22m /x");

        assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert!(!line.spans[1].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn the_extended_forms_are_honoured() {
        // The program picked this colour on purpose. Rounding it to the
        // sixteen would be Kobune choosing on its behalf.
        let indexed = line("\u{1b}[38;5;208morange");
        assert_eq!(indexed.spans[0].style.fg, Some(Color::Indexed(208)));

        let rgb = line("\u{1b}[48;2;12;34;56mground");
        assert_eq!(rgb.spans[0].style.bg, Some(Color::Rgb(12, 34, 56)));
    }

    #[test]
    fn a_sequence_that_is_not_about_colour_leaves_nothing_behind() {
        // Clearing the line, moving the cursor, setting the window
        // title. A pane has no cursor and its rows are already rows.
        assert_eq!(text(&line("\u{1b}[2K\u{1b}[1;5Hplain")), "plain");
        assert_eq!(text(&line("\u{1b}]0;a title\u{7}plain")), "plain");
        assert_eq!(text(&line("\u{1b}]0;a title\u{1b}\\plain")), "plain");
    }

    #[test]
    fn a_progress_bar_shows_where_it_got_to() {
        // Carriage returns are how one redraws itself. What a terminal
        // would be showing is the part after the last of them.
        assert_eq!(text(&line("10%\r55%\r100%")), "100%");
        assert_eq!(text(&line("done\r")), "done");
    }

    #[test]
    fn tabs_land_where_a_terminal_would_put_them() {
        // Output aligned into columns for a terminal has to stay aligned
        // here, or the pane is the one thing that shears it.
        assert_eq!(text(&line("ab\tc")), "ab      c");
        assert_eq!(text(&line("abcdefgh\tc")), "abcdefgh        c");
    }

    #[test]
    fn a_line_cut_off_mid_sequence_is_not_printed_as_text() {
        // The daemon splits output into lines, and a sequence can be
        // split with them.
        assert_eq!(text(&line("done \u{1b}[3")), "done ");
    }

    #[test]
    fn an_empty_line_stays_empty() {
        assert_eq!(text(&line("")), "");
        assert_eq!(text(&line("\u{1b}[0m")), "");
    }

    #[test]
    fn a_bare_reset_clears_everything_at_once() {
        // `ESC[m` is `ESC[0m`, and some programs write it that way.
        let line = line("\u{1b}[1;31mred\u{1b}[mplain");

        assert_eq!(line.spans[1].content, "plain");
        assert_eq!(line.spans[1].style.fg, None);
        assert!(line.spans[1].style.add_modifier.is_empty());
    }
}
