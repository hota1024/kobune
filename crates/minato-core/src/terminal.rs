//! What a program has made of the terminal it was given.
//!
//! A program that draws a full-screen interface says so once, in its first
//! few bytes: `ESC[?1049h` for the alternate screen, `ESC[?25l` to hide the
//! cursor, `ESC[?1000h` and friends to ask for mouse reports. It never says
//! it again — there is no reason to, since the terminal it is talking to
//! has not changed.
//!
//! **Except that under `minato logs -f` it has.** Attaching hands the
//! program a terminal it has never seen, minutes after it announced what it
//! wanted, and the announcement is long gone: the terminal sends no mouse
//! reports because nobody asked it to, so the wheel does nothing and the
//! frames land on the normal screen. Reading those announcements as they go
//! past is what makes them repeatable.
//!
//! Only [`DEC private modes`](Modes) are followed — the `ESC[?...h` and
//! `ESC[?...l` sequences — and only the ones that describe *what kind of
//! terminal the program thinks it has*. Everything else in the stream is a
//! record of a screen that no longer exists, and replaying it would draw a
//! picture the program is about to redraw anyway.

use std::collections::BTreeMap;

/// The modes worth carrying to a terminal that arrives late, and what a
/// terminal does when nobody has said otherwise.
///
/// A whitelist rather than everything seen, because not every private mode
/// is a state: `ESC[?2026h` opens a synchronised update and the matching
/// `l` closes it a frame later, so replaying the half that was caught would
/// freeze a display rather than describe one.
const TRACKED: &[(u16, bool)] = &[
    // Arrow keys as `ESC O A` rather than `ESC [ A`. Replayed because the
    // program reads whichever form it asked for, and gets neither if it
    // asked before this terminal existed.
    (1, false),
    // Wrap at the right margin.
    (7, true),
    // The cursor is visible.
    (25, true),
    // The alternate screen, in its three forms. A program uses one.
    (47, false),
    (1047, false),
    (1049, false),
    // Mouse reporting: which events are sent...
    (1000, false),
    (1002, false),
    (1003, false),
    // ...and how their coordinates are written. These are what a wheel
    // needs to reach the program at all.
    (1005, false),
    (1006, false),
    (1015, false),
    (1016, false),
    // The window gained or lost focus.
    (1004, false),
    // The wheel as arrow keys, for a program that never asked for a mouse.
    (1007, false),
    // A paste arrives wrapped in `ESC[200~` and `ESC[201~`.
    (2004, false),
];

/// What a terminal does with a mode nobody has set.
fn default_for(mode: u16) -> Option<bool> {
    TRACKED
        .iter()
        .find(|(tracked, _)| *tracked == mode)
        .map(|(_, default)| *default)
}

/// Where in a `ESC [ ? ... h` sequence the reader is.
///
/// Held between chunks: a terminal's output arrives in whatever sizes the
/// pipe hands over, and a sequence split across two of them is the ordinary
/// case rather than the exception.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Scan {
    /// Ordinary output.
    #[default]
    Ground,
    /// `ESC`.
    Escape,
    /// `ESC [`.
    Bracket,
    /// `ESC [ ?`, collecting mode numbers.
    Private,
    /// Some other escape sequence, being skipped to its end.
    Skipping,
}

/// The DEC private modes a program has set on its terminal.
///
/// Fed the bytes a program writes; asked afterwards what to tell a terminal
/// so that it matches. Only modes the program actually set are remembered,
/// and only where the program left them somewhere other than where a
/// terminal starts — so a program that draws plain text leaves this empty
/// and nothing is replayed.
#[derive(Debug, Clone, Default)]
pub struct Modes {
    /// Every tracked mode the program set, as it last set it.
    set: BTreeMap<u16, bool>,
    scan: Scan,
    /// The mode numbers collected so far in the sequence being read.
    parameters: Vec<u16>,
    /// The digits since the last `;`, if any.
    digits: Option<u16>,
}

impl Modes {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads a chunk of what a program wrote, remembering what it changed.
    ///
    /// Everything that is not a private mode sequence is passed over: this
    /// is a reader, not an emulator, and the screen is the program's to
    /// redraw.
    pub fn watch(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            // An escape anywhere starts a new sequence. A stream that was
            // cut mid-sequence — a subscriber that fell behind, a chunk
            // that never came — must not swallow the one after it.
            if byte == 0x1b {
                self.scan = Scan::Escape;
                self.forget_parameters();
                continue;
            }

            match self.scan {
                Scan::Ground => {}
                Scan::Escape => {
                    self.scan = if byte == b'[' {
                        Scan::Bracket
                    } else {
                        Scan::Ground
                    };
                }
                Scan::Bracket => {
                    self.scan = if byte == b'?' {
                        Scan::Private
                    } else if is_final(byte) {
                        Scan::Ground
                    } else {
                        Scan::Skipping
                    };
                }
                Scan::Private => self.read_private(byte),
                Scan::Skipping => {
                    if is_final(byte) {
                        self.scan = Scan::Ground;
                    }
                }
            }
        }
    }

    /// One byte of a `ESC [ ? ... h` sequence.
    fn read_private(&mut self, byte: u8) {
        match byte {
            b'0'..=b'9' => {
                let digit = u16::from(byte - b'0');
                // A number longer than a mode number is not one. Saturating
                // rather than wrapping keeps it out of the tracked range
                // instead of landing on some unrelated mode.
                self.digits = Some(
                    self.digits
                        .unwrap_or(0)
                        .saturating_mul(10)
                        .saturating_add(digit),
                );
            }
            b';' => {
                self.parameters.push(self.digits.take().unwrap_or(0));
            }
            b'h' | b'l' => {
                let on = byte == b'h';
                if let Some(last) = self.digits.take() {
                    self.parameters.push(last);
                }

                for mode in std::mem::take(&mut self.parameters) {
                    if default_for(mode).is_some() {
                        self.set.insert(mode, on);
                    }
                }

                self.scan = Scan::Ground;
            }
            _ => {
                // Some other private sequence — a query, a report. Skipped
                // whole, so that its final byte cannot be mistaken for the
                // end of a mode change.
                self.forget_parameters();
                self.scan = if is_final(byte) {
                    Scan::Ground
                } else {
                    Scan::Skipping
                };
            }
        }
    }

    fn forget_parameters(&mut self) {
        self.parameters.clear();
        self.digits = None;
    }

    /// The modes the program left somewhere other than where a terminal
    /// starts.
    fn changed(&self) -> impl Iterator<Item = (u16, bool)> + '_ {
        self.set.iter().filter_map(|(&mode, &on)| {
            let default = default_for(mode)?;
            (on != default).then_some((mode, on))
        })
    }

    /// Whether there is anything to say to a terminal at all.
    pub fn is_empty(&self) -> bool {
        self.changed().next().is_none()
    }

    /// What to send a terminal so that it matches the one the program
    /// believes it has.
    ///
    /// Empty for a program that only ever printed text, which is most of
    /// them: nothing is sent where nothing was changed.
    pub fn preamble(&self) -> Vec<u8> {
        self.changed()
            .flat_map(|(mode, on)| sequence(mode, on))
            .collect()
    }

    /// What to send to put every mode this touched back to its default.
    ///
    /// For whoever detaches: a mouse mode left on writes a report into the
    /// shell every time the pointer moves, and the alternate screen left on
    /// hides the prompt behind the program's last frame.
    pub fn restoration(&self) -> Vec<u8> {
        self.changed()
            .filter_map(|(mode, _)| Some((mode, default_for(mode)?)))
            .flat_map(|(mode, default)| sequence(mode, default))
            .collect()
    }
}

/// `ESC [ ? <mode> h`, or `l`.
fn sequence(mode: u16, on: bool) -> Vec<u8> {
    format!("\x1b[?{mode}{}", if on { 'h' } else { 'l' }).into_bytes()
}

/// Whether this byte ends an escape sequence.
fn is_final(byte: u8) -> bool {
    (0x40..=0x7e).contains(&byte)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn after(chunks: &[&[u8]]) -> Modes {
        let mut modes = Modes::new();
        for chunk in chunks {
            modes.watch(chunk);
        }
        modes
    }

    fn text(bytes: Vec<u8>) -> String {
        String::from_utf8(bytes).expect("escape sequences are ASCII")
    }

    #[test]
    fn plain_output_changes_nothing() {
        let modes = after(&[b"turbo run dev\r\n\x1b[32mready\x1b[0m\r\n"]);
        assert!(modes.is_empty());
        assert!(modes.preamble().is_empty());
        assert!(modes.restoration().is_empty());
    }

    #[test]
    fn what_crossterm_asks_for_comes_back() {
        // The exact bytes `EnableMouseCapture` and `EnterAlternateScreen`
        // write, which is what turborepo's TUI opens with.
        let modes = after(&[b"\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1015h\x1b[?1006h\x1b[?1049h"]);

        assert_eq!(
            text(modes.preamble()),
            "\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h\x1b[?1015h\x1b[?1049h"
        );
    }

    #[test]
    fn a_sequence_split_across_chunks_is_still_read() {
        // A terminal's output arrives in whatever sizes the pipe gives, so
        // this is the ordinary case rather than a corner of one.
        let modes = after(&[b"\x1b[?10", b"06", b"h"]);
        assert_eq!(text(modes.preamble()), "\x1b[?1006h");
    }

    #[test]
    fn several_modes_in_one_sequence() {
        let modes = after(&[b"\x1b[?1000;1006;1049h"]);
        assert_eq!(text(modes.preamble()), "\x1b[?1000h\x1b[?1006h\x1b[?1049h");
    }

    #[test]
    fn a_mode_turned_back_off_is_not_replayed() {
        let modes = after(&[b"\x1b[?1000h", b"\x1b[?1000l"]);
        assert!(modes.is_empty(), "the terminal is where it started");
    }

    #[test]
    fn a_mode_that_starts_on_is_replayed_when_it_is_turned_off() {
        // Hiding the cursor is a change; showing it is not.
        let modes = after(&[b"\x1b[?25l"]);
        assert_eq!(text(modes.preamble()), "\x1b[?25l");
        assert_eq!(text(modes.restoration()), "\x1b[?25h");
    }

    #[test]
    fn restoration_undoes_exactly_what_was_set() {
        let modes = after(&[b"\x1b[?1049h\x1b[?25l\x1b[?1006h"]);
        assert_eq!(text(modes.restoration()), "\x1b[?25h\x1b[?1006l\x1b[?1049l");
    }

    #[test]
    fn untracked_modes_are_left_alone() {
        // A synchronised update is a bracket around one frame, not a state
        // to put a late terminal into.
        let modes = after(&[b"\x1b[?2026h"]);
        assert!(modes.is_empty());
    }

    #[test]
    fn ordinary_escape_sequences_are_passed_over() {
        // Colour, cursor movement, erase — none of them private modes, and
        // one of them ends in `h` (`ESC[4h`, insert mode).
        let modes = after(&[b"\x1b[1;32m\x1b[2J\x1b[10;20H\x1b[4h\x1b]0;title\x07"]);
        assert!(modes.is_empty());
    }

    #[test]
    fn a_cut_sequence_does_not_swallow_the_next_one() {
        // What a subscriber that fell behind sees: half a sequence, then
        // whatever came after the gap.
        let modes = after(&[b"\x1b[?10", b"\x1b[?1006h"]);
        assert_eq!(text(modes.preamble()), "\x1b[?1006h");
    }

    #[test]
    fn a_private_query_is_not_mistaken_for_a_mode_change() {
        // `ESC[?1006$p` asks what the mode is. Its `p` ends it, and the
        // `1006` in it was never set.
        let modes = after(&[b"\x1b[?1006$p"]);
        assert!(modes.is_empty());
    }

    #[test]
    fn a_number_too_long_to_be_a_mode_lands_on_no_mode() {
        let modes = after(&[b"\x1b[?999999999h"]);
        assert!(modes.is_empty());
    }
}
