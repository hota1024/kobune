//! Lending this terminal to a service, for `minato logs`.
//!
//! Everything here is about getting out of the way. A program drawing a
//! full-screen interface — turborepo's, most test runners' — needs the
//! bytes it writes to arrive unaltered and the keys pressed to arrive
//! unread, so the terminal goes into raw mode and this passes both
//! directions through without looking at them.
//!
//! The one exception is the detach sequence, which has to be caught here:
//! ctrl-c belongs to the program, so there has to be something else that
//! means "give me my terminal back".

use std::io::{IsTerminal, Read, Write};

use minato_client::Typed;
use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

/// Ctrl-P, then Ctrl-Q: the keys that detach.
///
/// Docker's sequence, deliberately. Anyone who has detached from a
/// container before will try this one first.
const DETACH: [u8; 2] = [0x10, 0x11];

/// Whether this terminal is one that can be handed over.
///
/// Both halves matter: input from a pipe cannot be put into raw mode, and
/// output that is redirected — or on a terminal that shows escape
/// sequences as text, which is what [`crate::ui::is_interactive`] rules
/// out — must not be given a full-screen program's drawing.
pub fn is_a_terminal() -> bool {
    std::io::stdin().is_terminal() && crate::ui::is_interactive()
}

/// What to say before the program takes the screen.
///
/// On stderr, and before raw mode: a line printed after the handover
/// would be drawn over by the program's first frame.
pub fn announce(service: &str) {
    eprintln!(
        "attached to {service} — ctrl-p ctrl-q to detach, leaving it \
         running. Everything else, ctrl-c included, goes to the program"
    );
}

/// Undoes what a full-screen program was in the middle of.
///
/// **Best effort, and only after an attachment.** A program that drew a
/// full-screen interface was on the alternate screen with the cursor
/// hidden, and detaching leaves it that way: the shell prompt lands on
/// top of the last frame with nothing to type at. Leaving the alternate
/// screen and showing the cursor is what a terminal multiplexer does when
/// a pane goes, and on a program that used neither it costs nothing.
pub fn restore() {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(b"\x1b[?1049l\x1b[?25h");
    let _ = out.flush();
}

/// The terminal, for as long as a service has it.
///
/// Raw mode is entered when this is built and left when it is dropped, so
/// an error on any path still gives the terminal back.
pub struct Session {
    /// The keyboard is read on a thread that nothing waits for, so only
    /// this one is held.
    resizes: JoinHandle<()>,
}

impl Session {
    /// Takes the terminal and starts passing it on.
    pub fn start(typed: UnboundedSender<Typed>) -> std::io::Result<Self> {
        enable_raw_mode()?;

        std::thread::spawn({
            let typed = typed.clone();
            move || pass_on_keys(typed)
        });

        Ok(Self {
            resizes: tokio::spawn(pass_on_resizes(typed)),
        })
    }

    /// Writes what the service's terminal produced, byte for byte.
    ///
    /// Flushed every time. A full-screen program's output only makes sense
    /// at the moment it arrives — a cursor move held back in a buffer is a
    /// screen drawn in the wrong order.
    pub fn show(bytes: &[u8]) {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(bytes);
        let _ = out.flush();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.resizes.abort();
        let _ = disable_raw_mode();
    }
}

/// The state between one read of the terminal and the next.
///
/// All it holds is whether the last byte was the first half of the detach
/// sequence, which is what makes the sequence recognisable when its two
/// keys land in separate reads — the usual case, since they are typed one
/// after the other.
#[derive(Default)]
struct Keys {
    /// A ctrl-p seen and **held back rather than sent**: one that turns
    /// out to begin a detach must not reach the program first. It is
    /// passed on as soon as the next key proves it was meant for the
    /// program, the same small delay `docker attach` has.
    half_way: bool,
}

/// What one read of the terminal amounts to.
struct Filtered {
    /// The bytes the program should see.
    keys: Vec<u8>,
    /// Whether the detach sequence was completed in this chunk.
    detach: bool,
}

impl Keys {
    fn filter(&mut self, chunk: &[u8]) -> Filtered {
        let mut keys = Vec::with_capacity(chunk.len() + 1);

        for &byte in chunk {
            if self.half_way {
                self.half_way = false;

                if byte == DETACH[1] {
                    // Anything after the sequence was typed at a terminal
                    // that is no longer lent out, so it goes no further.
                    return Filtered { keys, detach: true };
                }

                keys.push(DETACH[0]);

                // Two in a row: the first was for the program, and the
                // second may still begin a detach.
                if byte == DETACH[0] {
                    self.half_way = true;
                    continue;
                }
            } else if byte == DETACH[0] {
                self.half_way = true;
                continue;
            }

            keys.push(byte);
        }

        Filtered {
            keys,
            detach: false,
        }
    }
}

/// Reads this terminal and sends it on, until asked to detach.
///
/// Returning drops the sender, which is how the rest of the program is
/// told that the person has left.
///
/// **A thread of its own, not a task.** A read of standard input cannot
/// be cancelled once it has started, so on a task this would sit parked
/// in the middle of one when the session ended from the other side — and
/// the runtime waits for its blocking work before the process may exit.
/// The command would appear to hang until someone pressed a key. Nothing
/// waits for a thread.
fn pass_on_keys(typed: UnboundedSender<Typed>) {
    let mut stdin = std::io::stdin().lock();
    let mut buffer = [0u8; 1024];
    let mut keys = Keys::default();

    loop {
        let Ok(count) = stdin.read(&mut buffer) else {
            return;
        };

        if count == 0 {
            return;
        }

        let filtered = keys.filter(&buffer[..count]);

        if !filtered.keys.is_empty() && typed.send(Typed::Keys(filtered.keys)).is_err() {
            return;
        }

        if filtered.detach {
            return;
        }
    }
}

/// Tells the service when this window changes size.
async fn pass_on_resizes(typed: UnboundedSender<Typed>) {
    let mut resized =
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change()) {
            Ok(signal) => signal,
            // **Silently.** The size sent with the request still stands, so
            // all that is lost is following a window someone drags — and the
            // terminal is in raw mode by now, where a printed warning would
            // come out as a staircase across the program's display.
            Err(_) => return,
        };

    // Dragging a window emits a stream of signals, and most of them
    // report the size the last one did. Each one that goes on costs a
    // message, a hop through the daemon and a call into the runtime.
    let mut last = crate::ui::window();

    while resized.recv().await.is_some() {
        let Some(window) = crate::ui::window() else {
            continue;
        };

        if last == Some(window) {
            continue;
        }
        last = Some(window);

        if typed.send(Typed::Resize(window)).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    /// Drives the real filter over a sequence of reads.
    ///
    /// The pump itself reads standard input, which a test cannot feed, so
    /// what is exercised is everything the pump decides with.
    fn filter(chunks: &[&[u8]]) -> (Vec<u8>, bool) {
        let mut keys = Keys::default();
        let mut passed = Vec::new();

        for chunk in chunks {
            let filtered = keys.filter(chunk);
            passed.extend(filtered.keys);

            if filtered.detach {
                return (passed, true);
            }
        }

        (passed, false)
    }

    #[test]
    fn ordinary_keys_pass_straight_through() {
        let (passed, detached) = filter(&[b"turbo\r\x1b[A"]);
        assert_eq!(passed, b"turbo\r\x1b[A");
        assert!(!detached);
    }

    #[test]
    fn ctrl_c_belongs_to_the_program() {
        // The whole point of having a detach sequence at all: a TUI reads
        // ctrl-c as "quit me", and it has to arrive.
        let (passed, detached) = filter(&[&[0x03]]);
        assert_eq!(passed, vec![0x03]);
        assert!(!detached);
    }

    #[test]
    fn the_detach_sequence_detaches() {
        let (passed, detached) = filter(&[&[0x10, 0x11]]);
        assert!(passed.is_empty(), "neither key reaches the program");
        assert!(detached);
    }

    #[test]
    fn the_sequence_is_recognised_across_two_reads() {
        // Keys arrive as they are typed, so the two halves usually land in
        // separate reads.
        let (passed, detached) = filter(&[&[0x10], &[0x11]]);
        assert!(passed.is_empty());
        assert!(detached);
    }

    #[test]
    fn a_ctrl_p_meant_for_the_program_still_arrives() {
        // Ctrl-P is a real key — previous line, in a shell — and holding
        // it back for good would swallow it.
        let (passed, detached) = filter(&[&[0x10], b"x"]);
        assert_eq!(passed, vec![0x10, b'x']);
        assert!(!detached);
    }

    #[test]
    fn two_ctrl_ps_then_a_detach() {
        let (passed, detached) = filter(&[&[0x10, 0x10, 0x11]]);
        assert_eq!(passed, vec![0x10], "the first one was for the program");
        assert!(detached);
    }

    #[tokio::test]
    async fn dropping_the_keyboard_is_how_leaving_is_reported() {
        let (typed, mut received) = mpsc::unbounded_channel::<Typed>();
        drop(typed);

        assert!(
            received.recv().await.is_none(),
            "the client reads this as the person having detached"
        );
    }
}
