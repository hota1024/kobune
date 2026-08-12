//! Lending this terminal to a service, for `minato logs`.
//!
//! Everything here is about getting out of the way. A program drawing a
//! full-screen interface — turborepo's, most test runners' — needs the
//! bytes it writes to arrive unaltered and the keys pressed to arrive
//! unread, so the terminal goes into raw mode and this passes both
//! directions through without altering either.
//!
//! Two things are read on the way past, and only read. The detach sequence
//! has to be caught here: ctrl-c belongs to the program, so there has to be
//! something else that means "give me my terminal back". And what the
//! service makes of this terminal is noted — the alternate screen, the
//! mouse — so that leaving can put it back.

use std::io::{IsTerminal, Read, Write};

use minato_api::{Typed, Window};
use ratatui::crossterm::terminal::{LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use tokio::sync::mpsc::{UnboundedSender, WeakUnboundedSender};
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

/// The terminal this session could lend, and how big it is.
///
/// One answer rather than two, so "a terminal, of no particular size"
/// cannot be expressed: a size is what the far end will draw to.
pub fn offered_window() -> Option<Window> {
    if !is_a_terminal() {
        return None;
    }

    crate::ui::window()
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

/// This terminal, for as long as a service is drawing on it.
///
/// It holds what the service has made of it — the alternate screen, a
/// hidden cursor, the mouse reporting a full-screen program asked for —
/// read out of the bytes on their way to the screen. Detaching is then a
/// matter of putting back what was changed rather than guessing: a mouse
/// mode left on writes a report into the shell every time the pointer
/// moves.
#[derive(Default)]
pub struct Screen {
    modes: minato_core::Modes,
}

impl Screen {
    pub fn new() -> Self {
        Self::default()
    }

    /// Writes what the service's terminal produced, byte for byte.
    ///
    /// Flushed every time. A full-screen program's output only makes sense
    /// at the moment it arrives — a cursor move held back in a buffer is a
    /// screen drawn in the wrong order.
    pub fn show(&mut self, bytes: &[u8]) {
        self.modes.watch(bytes);

        let mut out = std::io::stdout().lock();
        let _ = out.write_all(bytes);
        let _ = out.flush();
    }

    /// Undoes what a full-screen program was in the middle of.
    ///
    /// **Best effort, and only after an attachment.** Every mode the
    /// service set goes back to what a terminal does without it, and then
    /// — whatever was or was not seen — the alternate screen is left and
    /// the cursor shown. That last part is the floor: a program that
    /// entered the alternate screen before anyone attached announced it to
    /// a terminal that no longer exists, and without this the shell prompt
    /// lands on top of its final frame with nothing to type at.
    pub fn restore(&self) {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(&self.modes.restoration());
        let _ = out.flush();
        drop(out);

        let _ = ratatui::crossterm::execute!(
            std::io::stdout(),
            LeaveAlternateScreen,
            ratatui::crossterm::cursor::Show
        );
    }
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

        let (keyboard, window) = split(typed);
        std::thread::spawn(move || pass_on_keys(keyboard));

        Ok(Self {
            resizes: tokio::spawn(pass_on_resizes(window)),
        })
    }
}

/// The keyboard's end of the channel, and the window watcher's.
///
/// **The keyboard holds the only sender.** Putting it down is how the rest
/// of the program is told that the person detached, and a second one held
/// by the watcher would mean it was never put down: the channel would stay
/// open with nobody reading the terminal, and ctrl-p ctrl-q would leave
/// `minato logs` running rather than end it. Weak, so a window that changes
/// size can still be reported without the watcher keeping the session alive
/// on its own.
///
/// What keeps it from coming back is [`pass_on_resizes`]'s signature: a
/// watcher that cannot be handed a strong sender cannot hold one. This
/// exists so that the pairing can also be shown in a test, since
/// [`Session::start`] needs a real terminal for raw mode and there is none
/// under `cargo test`.
fn split(typed: UnboundedSender<Typed>) -> (UnboundedSender<Typed>, WeakUnboundedSender<Typed>) {
    let window = typed.downgrade();
    (typed, window)
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
///
/// **Weakly.** This outlives nothing: a window that changes size after the
/// person has detached is no longer anybody's to report.
async fn pass_on_resizes(typed: WeakUnboundedSender<Typed>) {
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

        // Taken for the send and no longer, so that this never holds the
        // channel open across a wait.
        let Some(typed) = typed.upgrade() else {
            return;
        };

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
        let (keyboard, window) = split(typed);

        // What `pass_on_keys` does when it reads ctrl-p ctrl-q.
        drop(keyboard);

        assert!(
            received.recv().await.is_none(),
            "the client reads this as the person having detached"
        );

        // The window watcher used to be given a clone, so the channel
        // stayed open, `call_attached` waited on for ever, and ctrl-p
        // ctrl-q left `minato logs` running instead of ending it.
        assert!(
            window.upgrade().is_none(),
            "the window watcher must not hold the session open by itself"
        );
    }
}
