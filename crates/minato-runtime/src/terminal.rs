//! A terminal opened on this side of a container.
//!
//! **Only Apple Container needs this.** Docker's API hands out a terminal
//! over HTTP, so nothing local is involved; the `container` CLI has no
//! `attach` at all, and `container start --attach --interactive` is the
//! only way to reach a container's stdin. That command puts *its own*
//! standard input into raw mode, so it fails with `Inappropriate ioctl for
//! device` unless what it is given is a real terminal. The daemon
//! therefore opens one, keeps the near end, and runs the CLI on the far
//! end.
//!
//! One terminal is opened per service when it starts, and lives as long as
//! the container does. Everyone who attaches later shares it, so what one
//! person types is seen by the others — the same as two `docker attach`es
//! on one container.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use minato_core::Modes;
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;
use tokio::sync::{broadcast, mpsc};

/// How much output to keep for a subscriber that falls behind.
///
/// A subscriber that falls further behind than this loses the bytes in
/// between, which a full-screen program repairs on its next redraw. The
/// alternative — an unbounded queue — would grow without limit behind a
/// client that stopped reading.
const OUTPUT_BACKLOG: usize = 512;

/// The size to open the terminal at.
///
/// **Apple Container reads it once**, when the process is attached, and
/// ignores later changes: measured against `container` 1.2, where a
/// resize on this side never reached the program inside. So what
/// [`crate::runtime::DEFAULT_WINDOW`] says here is the size a full-screen
/// program will see for its whole life.
use crate::runtime::DEFAULT_WINDOW;

/// A pseudo-terminal with a process attached to its far end.
pub(crate) struct Terminal {
    /// What has been typed, on its way to the far end.
    input: mpsc::UnboundedSender<Vec<u8>>,

    /// What the far end has written, for whoever is listening.
    output: broadcast::Sender<Vec<u8>>,

    /// What the program has made of this terminal.
    ///
    /// Kept because [`subscribe`](Self::subscribe) replays nothing: a
    /// program says `ESC[?1049h` and `ESC[?1000h` once, in its first
    /// bytes, and someone who attaches an hour later needs telling. Read
    /// from every chunk on its way past, so it costs nothing when nobody
    /// is listening.
    modes: Arc<Mutex<Modes>>,
}

impl Terminal {
    /// Opens a terminal and runs `command` on the far end of it.
    ///
    /// The child's three streams are all the terminal, which is what makes
    /// it a terminal as far as the child is concerned. Output is drained
    /// from the moment it starts, whether or not anyone is listening: an
    /// undrained terminal fills up, and a program writing into a full one
    /// stops dead.
    pub(crate) fn open(mut command: tokio::process::Command) -> std::io::Result<Self> {
        let (master, slave) = open_pty(DEFAULT_WINDOW.cols, DEFAULT_WINDOW.rows)?;

        let stdin = slave.try_clone()?;
        let stdout = slave.try_clone()?;
        let mut child = command
            .stdin(Stdio::from(stdin))
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(slave))
            .spawn()?;

        // **The command owns this side's copies of the far end**, and only
        // gives them up when it is dropped. Held on to, they would keep
        // the terminal open after the child exited, and the read below
        // would wait for an end that never came.
        drop(command);

        set_nonblocking(&master)?;

        let (output, _) = broadcast::channel(OUTPUT_BACKLOG);
        let (input, mut typed) = mpsc::unbounded_channel::<Vec<u8>>();

        let modes = Arc::new(Mutex::new(Modes::new()));

        let published = output.clone();
        let watched = modes.clone();
        tokio::spawn(async move {
            let fd = match AsyncFd::new(master) {
                Ok(fd) => fd,
                Err(err) => {
                    tracing::warn!("cannot watch the terminal: {err}");
                    return;
                }
            };

            let mut buffer = vec![0u8; 8192];
            loop {
                tokio::select! {
                    read = read_some(&fd, &mut buffer) => match read {
                        // The far end closed: the process exited, or the
                        // container it was attached to went away.
                        Ok(0) | Err(_) => break,
                        Ok(count) => {
                            // Read before it is handed on, and whether or
                            // not anyone is listening: the announcement
                            // this is looking for comes long before the
                            // first attachment. A lock that cannot be
                            // taken costs the replay and nothing else, so
                            // the output still goes on below.
                            if let Ok(mut watched) = watched.lock() {
                                watched.watch(&buffer[..count]);
                            }

                            // An error means nobody is listening, which is
                            // the normal state of a service nobody has
                            // attached to.
                            let _ = published.send(buffer[..count].to_vec());
                        }
                    },
                    keys = typed.recv() => match keys {
                        Some(keys) => {
                            if write_all(&fd, &keys).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    },
                }
            }

            // Reaped here rather than left behind. The process is a
            // `container start` that has already exited by the time the
            // terminal closes; without a wait it stays a zombie.
            let _ = child.wait().await;
        });

        Ok(Self {
            input,
            output,
            modes,
        })
    }

    /// The output from now on. Nothing already written is replayed.
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<Vec<u8>> {
        self.output.subscribe()
    }

    /// What to tell a terminal so that it matches the one the program
    /// believes it is writing to.
    ///
    /// **Nothing, rather than a panic, if the reader task died holding the
    /// lock.** Every other step of an attachment degrades to "no mouse, no
    /// alternate screen"; this one taking the runtime's container
    /// bookkeeping down with it — `attach` holds that lock too — would be
    /// the whole session lost for the sake of a preamble.
    pub(crate) fn preamble(&self) -> Vec<u8> {
        self.modes
            .lock()
            .map(|modes| modes.preamble())
            .unwrap_or_default()
    }

    /// Where to send keystrokes.
    pub(crate) fn keyboard(&self) -> Keyboard {
        Keyboard {
            keys: self.input.clone(),
        }
    }

    /// Whether the far end is still there.
    pub(crate) fn is_open(&self) -> bool {
        !self.input.is_closed()
    }
}

/// The typing end of a [`Terminal`].
///
/// Writing never waits: keystrokes are queued for the task that owns the
/// terminal, which is the only thing allowed to touch the descriptor. A
/// terminal whose far end has gone takes what is typed and drops it, the
/// same as typing at a window whose program has exited.
pub(crate) struct Keyboard {
    keys: mpsc::UnboundedSender<Vec<u8>>,
}

impl tokio::io::AsyncWrite for Keyboard {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let _ = self.keys.send(buf.to_vec());
        std::task::Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        // **The terminal outlives the person typing at it.** Closing it
        // here would take the service's stdin away for good the first time
        // anyone stopped watching.
        std::task::Poll::Ready(Ok(()))
    }
}

/// Opens a pseudo-terminal, near end first.
fn open_pty(cols: u16, rows: u16) -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut master: RawFd = -1;
    let mut slave: RawFd = -1;
    let mut size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    // A raw pointer rather than a reference, because the two platforms
    // disagree about its constness — macOS takes `*mut winsize`, Linux
    // `*const` — and a `*mut` coerces to either.
    let requested: *mut libc::winsize = &mut size;

    // SAFETY: `openpty` writes one descriptor through each of the first
    // two pointers and reads the window size through the last. The two
    // name arguments are optional and left null.
    let result = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            requested,
        )
    };

    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }

    // SAFETY: both descriptors were just created by `openpty` and are not
    // owned anywhere else.
    Ok(unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) })
}

/// Puts a descriptor into non-blocking mode, as [`AsyncFd`] requires.
fn set_nonblocking(fd: &OwnedFd) -> std::io::Result<()> {
    // SAFETY: `fd` is owned and open, and both calls only read and write
    // its flags.
    unsafe {
        let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
        if flags < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }

    Ok(())
}

/// Reads whatever is there, waiting for something to be.
///
/// `async_io` is what retries a descriptor that said it was ready and
/// then was not — the loop this would otherwise be.
async fn read_some(fd: &AsyncFd<OwnedFd>, buffer: &mut [u8]) -> std::io::Result<usize> {
    fd.async_io(Interest::READABLE, |inner| {
        // SAFETY: the buffer is borrowed for the length passed, and the
        // descriptor is owned by the caller.
        let count = unsafe {
            libc::read(
                inner.as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                buffer.len() as libc::size_t,
            )
        };

        if count < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(count as usize)
        }
    })
    .await
}

/// Writes every byte, or gives up on the first real failure.
async fn write_all(fd: &AsyncFd<OwnedFd>, mut bytes: &[u8]) -> std::io::Result<()> {
    while !bytes.is_empty() {
        let written = fd
            .async_io(Interest::WRITABLE, |inner| {
                // SAFETY: as in `read_some`, for a borrow that only reads.
                let count = unsafe {
                    libc::write(
                        inner.as_raw_fd(),
                        bytes.as_ptr().cast(),
                        bytes.len() as libc::size_t,
                    )
                };

                if count < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(count as usize)
                }
            })
            .await?;

        bytes = &bytes[written..];
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn carries_output_from_the_far_end() {
        let mut command = tokio::process::Command::new("sh");
        command.arg("-c").arg("echo hello");

        let terminal = Terminal::open(command).expect("opens");
        let mut output = terminal.subscribe();

        let chunk = tokio::time::timeout(std::time::Duration::from_secs(5), output.recv())
            .await
            .expect("does not time out")
            .expect("a chunk arrives");

        assert!(
            String::from_utf8_lossy(&chunk).contains("hello"),
            "got: {chunk:?}"
        );
    }

    #[tokio::test]
    async fn the_far_end_is_a_terminal() {
        // The whole point: `container start -a -i` refuses anything else.
        let mut command = tokio::process::Command::new("sh");
        command.arg("-c").arg("test -t 0 && test -t 1 && echo yes");

        let terminal = Terminal::open(command).expect("opens");
        let mut output = terminal.subscribe();

        let chunk = tokio::time::timeout(std::time::Duration::from_secs(5), output.recv())
            .await
            .expect("does not time out")
            .expect("a chunk arrives");

        assert!(
            String::from_utf8_lossy(&chunk).contains("yes"),
            "got: {chunk:?}"
        );
    }

    #[tokio::test]
    async fn what_is_typed_reaches_the_far_end() {
        let mut command = tokio::process::Command::new("sh");
        command.arg("-c").arg("read line; echo \"got:$line\"");

        let terminal = Terminal::open(command).expect("opens");
        let mut output = terminal.subscribe();
        {
            use tokio::io::AsyncWriteExt as _;
            let mut keyboard = terminal.keyboard();
            keyboard.write_all(b"ping\n").await.expect("takes it");
        }

        // The terminal echoes what is typed before the program answers, so
        // read until the answer turns up.
        let answered = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let mut seen = String::new();
            while let Ok(chunk) = output.recv().await {
                seen.push_str(&String::from_utf8_lossy(&chunk));
                if seen.contains("got:ping") {
                    return true;
                }
            }
            false
        })
        .await
        .expect("does not time out");

        assert!(answered);
    }

    #[tokio::test]
    async fn the_size_is_the_one_it_was_opened_with() {
        let mut command = tokio::process::Command::new("sh");
        command.arg("-c").arg("stty size");

        let terminal = Terminal::open(command).expect("opens");
        let mut output = terminal.subscribe();

        let chunk = tokio::time::timeout(std::time::Duration::from_secs(5), output.recv())
            .await
            .expect("does not time out")
            .expect("a chunk arrives");

        let text = String::from_utf8_lossy(&chunk).trim().to_string();
        assert_eq!(
            text,
            format!("{} {}", DEFAULT_WINDOW.rows, DEFAULT_WINDOW.cols),
            "got: {text}"
        );
    }

    #[tokio::test]
    async fn what_the_program_asked_of_the_terminal_is_kept() {
        // Nobody is subscribed while this runs, which is the point: the
        // announcement comes long before the first attachment, and an
        // attachment that arrives after it has to be told.
        let mut command = tokio::process::Command::new("sh");
        command
            .arg("-c")
            .arg("printf '\\033[?1049h\\033[?1006h\\033[?25l'");

        let terminal = Terminal::open(command).expect("opens");

        let preamble = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let preamble = terminal.preamble();
                if !preamble.is_empty() {
                    return preamble;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("does not time out");

        // In the order it asked, which is what makes it a replay.
        assert_eq!(
            String::from_utf8_lossy(&preamble),
            "\x1b[?1049h\x1b[?1006h\x1b[?25l"
        );
    }

    #[tokio::test]
    async fn a_program_that_only_prints_leaves_nothing_to_replay() {
        let mut command = tokio::process::Command::new("sh");
        command.arg("-c").arg("echo hello");

        let terminal = Terminal::open(command).expect("opens");
        let mut output = terminal.subscribe();

        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), output.recv())
            .await
            .expect("does not time out");

        assert!(terminal.preamble().is_empty());
    }

    #[tokio::test]
    async fn closing_the_far_end_closes_the_terminal() {
        let mut command = tokio::process::Command::new("sh");
        command.arg("-c").arg("exit 0");

        let terminal = Terminal::open(command).expect("opens");

        let closed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while terminal.is_open() {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await;

        assert!(closed.is_ok(), "the terminal should notice the process go");
    }
}
