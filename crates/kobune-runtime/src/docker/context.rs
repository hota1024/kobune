//! Packing a build context, and handing it to the request a chunk at a time.
//!
//! **Its own module because it is its own subsystem.** What is in here walks
//! a worktree, applies a `.dockerignore`, writes a tar and pushes it down a
//! channel under back-pressure. It meets the rest of `docker` at one point:
//! [`super::DockerRuntime::run_build`] asks [`stream_context`] for a body,
//! and [`packing_failure`] for the reason when there was one.
//!
//! Why it streams rather than packing into a `Vec` first is in
//! [`CONTEXT_CHUNK`], and it is not only about memory.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bytes::Bytes;

use crate::event::EventSink;
use crate::spec::BuildSpec;

/// Where a Dockerfile from outside the build context is placed in the tar.
///
/// Prefixed so it cannot collide with a real file in the context.
const DOCKERFILE_ENTRY: &str = ".kobune-dockerfile";

/// How much of the build context goes into one frame of the request body.
///
/// **What matters is that there is a bound at all.** `hyper` collects body
/// frames and hands them to `writev(2)` as one vector, and macOS refuses a
/// vector whose lengths sum past what an `int` holds — so a context of one
/// 3.3 GB frame failed the request outright, with `EINVAL` and no build.
/// See [`pack_context`].
///
/// 128 KiB is under a third of what `hyper` will buffer before writing, so
/// several frames still leave in one call and the bound costs nothing; and
/// small enough that what is waiting on the socket is a couple of megabytes
/// rather than the worktree.
const CONTEXT_CHUNK: usize = 128 * 1024;

/// How many chunks of context may be waiting for the socket.
///
/// The packer blocks once this many are queued, which is what keeps memory
/// flat however large the context turns out to be.
const CHUNKS_IN_FLIGHT: usize = 16;

/// How much context goes by between one progress line and the next.
///
/// **Not a line per chunk.** Every one is an event fanned out to whoever is
/// watching, and a 3 GB context is twenty-five thousand chunks. This is
/// often enough to watch a large context move and silent for every context
/// that is not a problem.
const CONTEXT_STRIDE: u64 = 64 * 1024 * 1024;

/// The size of build context that is worth remarking on.
///
/// **Not a limit.** The context streams, so a large one works. It is that a
/// context this size is almost always something nobody meant to send: the
/// repository that prompted all this named `node_modules` and `.next` in
/// its `.dockerignore` and not the two directories that held 3.34 GB
/// between them, and nothing said so — the build simply failed. `docker
/// build` prints the size of what it sends, and this is the reason to.
const A_CONTEXT_WORTH_MENTIONING: u64 = 512 * 1024 * 1024;

/// Tars a build context for the Docker API, less what `.dockerignore` says
/// to leave out.
///
/// **Written out as it is walked, never held whole.** The API takes the
/// context as a tar stream, and a stream is what this produces: a 3.3 GB
/// worktree used to be read into one `Vec<u8>`, handed to `hyper` as a
/// single body frame and offered to `writev(2)` as a single vector, which
/// macOS refuses once the lengths sum past what an `int` holds. `docker
/// build` never met the limit because it sends the context in pieces, so
/// the same Dockerfile built on the command line and failed here — with
/// `client error (SendRequest)` and nothing else. See [`ChunkWriter`].
///
/// **No builder does the filtering for us**: `docker build` reads the file
/// on the client side and leaves the excluded paths out of what it uploads,
/// and a `.dockerignore` that arrives *inside* the tar is one more file in
/// the context. Kobune is the client here, so it reads it. See
/// [`crate::dockerignore`].
///
/// `dockerfile` says where the Dockerfile is, which decides both what the
/// patterns may not take out and whether one has to be added.
fn pack_context<W: std::io::Write>(
    context: &Path,
    dockerfile: &Dockerfile,
    into: W,
) -> std::io::Result<W> {
    let ignore = crate::dockerignore::Ignore::for_context(context, dockerfile.inside())?;

    let mut builder = tar::Builder::new(into);
    builder.follow_symlinks(false);
    // `./`, which is what `append_dir_all` called the root. Every entry
    // below it is named without the prefix; `tar` strips a leading `./`
    // from a path it is given, so the two spellings are one name.
    builder.append_dir("./", context)?;

    pack_into(&mut builder, context, "", &ignore)?;

    // **Before the archive is finished, not after.** A tar ends with two
    // zero blocks and every reader stops there, so an entry written past
    // them is bytes nobody looks at. Adding it to a second builder wrapped
    // around the finished bytes — which is what this used to do — sent a
    // context with no Dockerfile in it and failed the build with "cannot
    // locate specified Dockerfile".
    if let Dockerfile::Outside(path) = dockerfile {
        let contents = std::fs::read(path)?;
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        builder.append_data(&mut header, DOCKERFILE_ENTRY, contents.as_slice())?;
    }

    builder.into_inner()
}

/// Why the context could not be packed, when that is what happened.
///
/// Taken rather than borrowed: a build asks twice — once when it fails and
/// once when it does not — and the answer is only there to be had once.
///
/// [`Packed::Cut`] is not a reason and never becomes one: a build that
/// failed on its own account stops reading, so asking after the failure
/// would otherwise turn every early failure over a large context into a
/// sentence about a channel.
pub(super) async fn packing_failure(packing: &mut Option<Packing>) -> Option<String> {
    match packing.take()?.await {
        Ok(Packed::Failed(err)) => Some(crate::error::with_causes(&err)),
        Ok(Packed::All | Packed::Cut) => None,
        // **A walk that died is a context that stopped.** The body ends
        // cleanly when the packer unwinds — the sender goes with it — so
        // the daemon sees a short tar rather than an error, and a build
        // whose `COPY` happened to be satisfied by what arrived first
        // would succeed on half a worktree. Reading this as "nothing to
        // report" is how that gets tagged and run.
        Err(err) => Some(format!("the walk of the context did not finish: {err}")),
    }
}

/// A `Write` that hands the packed context to the request, a chunk at a time.
///
/// **Blocking, on a thread that may block.** `tar` is synchronous and the
/// walk is disk-bound, so it runs under `spawn_blocking`; the send is what
/// puts the socket's back-pressure onto the walk, and is the reason memory
/// stays flat however large the context turns out to be.
struct ChunkWriter {
    sender: tokio::sync::mpsc::Sender<Chunk>,
    buffer: Vec<u8>,
    /// How much has gone. Reported when the context is packed, so a
    /// worktree that turns out to be enormous says so.
    sent: u64,
    /// Where the progress goes, and what the step it belongs to is called.
    events: EventSink,
    label: String,
    /// What `sent` was at the last progress line, so the lines come at a
    /// stride rather than one per chunk.
    announced: u64,
    /// Whether the context has already been called large. Once per build
    /// is enough, and a build can pack its context twice — so this belongs
    /// to the build rather than to this writer. See [`Remarked`].
    remarked: Remarked,
}

impl ChunkWriter {
    fn new(
        sender: tokio::sync::mpsc::Sender<Chunk>,
        events: EventSink,
        label: String,
        remarked: Remarked,
    ) -> Self {
        Self {
            sender,
            buffer: Vec::with_capacity(CONTEXT_CHUNK),
            sent: 0,
            events,
            label,
            announced: 0,
            remarked,
        }
    }

    /// Says how much of the context has gone, now and then.
    fn announce(&mut self) {
        // `swap` rather than a read then a write: one call says
        // "first one through" outright, and cannot drift into two.
        if self.sent >= A_CONTEXT_WORTH_MENTIONING && !self.remarked.swap(true, Ordering::Relaxed) {
            let message = format!(
                "the build context is over {}. Anything `.dockerignore` \
                 does not name is sent, and everything sent is read",
                kobune_core::size::bytes(A_CONTEXT_WORTH_MENTIONING)
            );
            tracing::warn!("{message}");
            self.events.warn(message);
        }

        if self.sent - self.announced < CONTEXT_STRIDE {
            return;
        }

        self.announced = self.sent;
        self.report();
    }

    /// One line naming what has gone so far.
    fn report(&self) {
        self.events.step_progress(
            "build",
            &self.label,
            format!(
                "sending the build context: {}",
                kobune_core::size::bytes(self.sent)
            ),
        );
    }

    fn emit(&mut self, chunk: Vec<u8>) -> std::io::Result<()> {
        self.sent += chunk.len() as u64;
        self.sender
            .blocking_send(Ok(Bytes::from(chunk)))
            // **Nobody is reading, so stop packing.** A cancelled `up`, or
            // a daemon that answered before it had taken the whole
            // context. Walking the rest of a worktree to feed a request
            // that has gone is work with nowhere to put it — which is what
            // packing into memory first meant doing, every time.
            //
            // Marked so it can be told from a file that could not be read:
            // this one is never the reason for anything, and reporting it
            // as one would bury the real error. See [`Packed::Cut`].
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, Cut))?;

        self.announce();
        Ok(())
    }
}

impl std::io::Write for ChunkWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buffer.extend_from_slice(buf);

        while self.buffer.len() >= CONTEXT_CHUNK {
            // `split_off` hands the leftover a `Vec` of its own size, and
            // putting that back would throw the reservation away — `tar`
            // writes in 8 KiB pieces, so every chunk would climb back to
            // 128 KiB through four more allocations. Over a 3 GB context
            // that is a hundred thousand of them on the path this exists
            // to keep quick.
            let chunk = self.buffer.split_off(CONTEXT_CHUNK);
            let chunk = std::mem::replace(&mut self.buffer, chunk);
            self.buffer
                .reserve(CONTEXT_CHUNK.saturating_sub(self.buffer.len()));
            self.emit(chunk)?;
        }

        Ok(buf.len())
    }

    /// **Called once the archive is finished, not before.** A tar ends with
    /// two zero blocks, and they are in the tail this pushes out.
    fn flush(&mut self) -> std::io::Result<()> {
        if !self.buffer.is_empty() {
            let chunk = std::mem::take(&mut self.buffer);
            self.emit(chunk)?;
        }

        Ok(())
    }
}

/// What became of the walk that packed the context.
pub(super) enum Packed {
    /// All of it went. How much is on the progress line the walk itself
    /// emitted — see [`ChunkWriter::report`] — rather than carried back
    /// here, because by the time the build ends nobody is waiting on it.
    All,
    /// The request stopped reading before the walk was done.
    ///
    /// **Never a reason on its own.** This is what a build that failed
    /// looks like from the packer's side: the daemon answers, the body is
    /// dropped, and the send has nowhere to go. Reporting it would replace
    /// the Dockerfile error the user needs with a sentence about a channel.
    Cut,
    /// A file in the context could not be read.
    Failed(std::io::Error),
}

/// What [`ChunkWriter::emit`] puts inside its error when the request has
/// stopped reading.
///
/// **A type rather than a message to compare against.** The message is the
/// part anything in the way is free to change: `tar` hands a writer's
/// error straight back today, but a version that added the path it was
/// writing would stop the comparison matching — and a cut would be
/// reported as [`Packed::Failed`], which is exactly the case
/// [`Packed::Cut`] exists to keep out of the user's way. A marker survives
/// being wrapped, and [`Cut::behind`] looks through the wrapping.
#[derive(Debug)]
struct Cut;

impl std::fmt::Display for Cut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the build stopped reading the context")
    }
}

impl std::error::Error for Cut {}

impl Cut {
    /// Whether `err` is this, or carries it somewhere underneath.
    fn behind(err: &std::io::Error) -> bool {
        let mut node = err
            .get_ref()
            .map(|inner| inner as &(dyn std::error::Error + 'static));

        while let Some(current) = node {
            if current.is::<Self>() {
                return true;
            }

            // `io::Error::source` reaches past the error it holds rather
            // than handing it back, so an `io::Error` wrapped inside
            // another is stepped through here instead.
            node = match current.downcast_ref::<std::io::Error>() {
                Some(nested) => nested.get_ref().map(|inner| inner as _),
                None => current.source(),
            };
        }

        false
    }
}

/// One piece of the packed context on its way to the daemon, or the
/// reason there will not be another. `std::result::Result` spelled out
/// because this module's own `Result` is [`crate::error::Result`].
pub(super) type Chunk = std::result::Result<Bytes, std::io::Error>;

/// Whether the context has already been called large, for one build.
///
/// Shared rather than owned by the writer: a build packs its context twice
/// when BuildKit turns out to be missing, and the second walk has to know
/// what the first one already said. `Arc` because the walk that reads it
/// runs under `spawn_blocking`.
pub(super) type Remarked = Arc<AtomicBool>;

/// The walk, still going.
pub(super) type Packing = tokio::task::JoinHandle<Packed>;

/// The packed context, as the request body reads it.
pub(super) type Chunks = tokio_stream::wrappers::ReceiverStream<Chunk>;

/// Packs the context on a blocking thread and hands back its chunks.
///
/// The walk and the upload run at once, so the daemon reads while the
/// worktree is still being read — and a build nobody is waiting for any
/// more stops packing rather than finishing into a dropped buffer.
pub(super) fn stream_context(
    context: &Path,
    dockerfile: &Dockerfile,
    label: &str,
    events: &EventSink,
    remarked: &Remarked,
) -> (Chunks, Packing) {
    let (sender, receiver) = tokio::sync::mpsc::channel(CHUNKS_IN_FLIGHT);

    let context = context.to_path_buf();
    let dockerfile = dockerfile.clone();
    let events = events.clone();
    let label = label.to_string();
    let remarked = Arc::clone(remarked);

    let packing = tokio::task::spawn_blocking(move || {
        let writer = ChunkWriter::new(sender.clone(), events, label, remarked);

        let packed = pack_context(&context, &dockerfile, writer)
            .and_then(|mut writer| std::io::Write::flush(&mut writer).map(|()| writer));

        match packed {
            Ok(writer) => {
                // **Once, whatever the size.** The lines above come at a
                // stride, so a context under it has said nothing yet — and
                // the size of what was sent is the thing worth knowing
                // when a build behaves oddly.
                writer.report();
                Packed::All
            }
            Err(err) if Cut::behind(&err) => Packed::Cut,
            Err(err) => {
                // **Down the body as well as back to the caller.** A tar
                // that simply stops is a tar the daemon would try to build
                // from; ending the body with an error is what makes the
                // request be abandoned instead. `io::Error` is not `Clone`,
                // so what goes down the body is a copy and what the caller
                // reports is the original.
                let copy = std::io::Error::new(err.kind(), err.to_string());
                let _ = sender.blocking_send(Err(copy));
                Packed::Failed(err)
            }
        }
    });

    (
        tokio_stream::wrappers::ReceiverStream::new(receiver),
        packing,
    )
}

/// Where the Dockerfile a build names actually is.
///
/// Docker names the Dockerfile by its path inside the tar, so the two cases
/// are packed differently: one is already in the context under its own name,
/// the other has to be put there under a reserved one.
///
/// Owned rather than borrowed, because the walk that reads it runs on a
/// blocking thread of its own and outlives the call that started it.
#[derive(Clone, Debug)]
pub(super) enum Dockerfile {
    /// In the context, at this path relative to its root.
    Inside(String),
    /// Elsewhere in the worktree. One context can build several images, so
    /// `dockerfile` is free to point outside it.
    Outside(std::path::PathBuf),
}

impl Dockerfile {
    /// Where the build spec says the Dockerfile is.
    ///
    /// Worked out before the packing rather than after, because
    /// `.dockerignore` has to be told which file not to leave out.
    pub(super) fn of(build: &BuildSpec) -> Self {
        match build.dockerfile.strip_prefix(&build.context) {
            Ok(relative) => Self::Inside(relative.to_string_lossy().to_string()),
            Err(_) => Self::Outside(build.dockerfile.clone()),
        }
    }

    /// Its path within the context, when it has one.
    ///
    /// What [`crate::dockerignore`] is told not to leave out — an outside
    /// one is added after the patterns have had their say, so there is
    /// nothing to spare.
    fn inside(&self) -> Option<&str> {
        match self {
            Self::Inside(path) => Some(path),
            Self::Outside(_) => None,
        }
    }

    /// The name the build asks the daemon for.
    pub(super) fn entry(&self) -> String {
        match self {
            Self::Inside(path) => path.to_string(),
            Self::Outside(_) => DOCKERFILE_ENTRY.to_string(),
        }
    }
}

/// Adds what is under `dir` and not left out, then does the same below it.
///
/// `prefix` is `dir` relative to the root of the context, empty at the top.
fn pack_into<W: std::io::Write>(
    builder: &mut tar::Builder<W>,
    dir: &Path,
    prefix: &str,
    ignore: &crate::dockerignore::Ignore,
) -> std::io::Result<()> {
    let mut entries = std::fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;

    // `read_dir` hands back whatever order the filesystem keeps, which two
    // machines holding the same files need not agree on. Sorting costs
    // nothing at this size and makes one context pack to one tar.
    entries.sort_by_key(std::fs::DirEntry::file_name);

    for entry in entries {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let path = entry.path();
        let kind = entry.file_type()?;

        let relative = match prefix.is_empty() {
            true => name.to_string(),
            false => format!("{prefix}/{name}"),
        };

        let excluded = ignore.excludes(&relative);

        // **A left-out directory is still walked when a `!` line names
        // something inside it.** The directory stays out of the tar either
        // way; skipping it whole would lose the exception.
        if excluded && !(kind.is_dir() && ignore.may_hold_an_exception(&relative)) {
            continue;
        }

        // **Only the three kinds git can carry.** `tar` refuses a socket
        // outright, which used to take a whole build down over a Rails
        // `tmp/sockets` or a database left running in the worktree.
        //
        // Docker's client skips sockets and packs fifos and device nodes;
        // this skips all three, which is a difference worth being straight
        // about. A context comes from a worktree, and git cannot store any
        // of them — so one that is there was made by something running,
        // and is a runtime artefact rather than a file a `COPY` wants.
        // Packing them faithfully would also mean building the headers by
        // hand: `tar`'s own path for a special file names the entry after
        // its absolute location on disk.
        if !excluded && (kind.is_file() || kind.is_dir() || kind.is_symlink()) {
            builder.append_path_with_name(&path, &*relative)?;
        }

        // A symlink is packed as itself rather than followed, so only a
        // real directory is descended into.
        if kind.is_dir() {
            pack_into(builder, &path, &relative, ignore)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every chunk the context went out in, and what the packer reported.
    async fn streamed(context: &Path, dockerfile: &Dockerfile) -> (Vec<Bytes>, Packed) {
        use futures::StreamExt;

        let (chunks, packing) = stream_context(
            context,
            dockerfile,
            "build",
            &EventSink::discard(),
            &Remarked::default(),
        );
        let mut kept = Vec::new();
        let mut failure = None;

        let mut chunks = chunks;
        while let Some(chunk) = chunks.next().await {
            match chunk {
                Ok(bytes) => kept.push(bytes),
                Err(err) => failure = Some(err),
            }
        }

        let reported = packing.await.expect("the packer ran");

        if let Some(err) = failure {
            assert!(
                matches!(reported, Packed::Failed(_)),
                "the body was ended with {err} and the packer reported nothing"
            );
        }

        (kept, reported)
    }

    /// **A context leaves in pieces, whatever size it is.**
    ///
    /// One `Bytes` for the whole context is one vector handed to
    /// `writev(2)`, and macOS refuses one whose lengths sum past what an
    /// `int` holds — so a 3.3 GB worktree failed the build outright with
    /// `client error (SendRequest)` and nothing else, while a 12 KB one
    /// beside it built every time. A file larger than one chunk is what
    /// says the writer splits rather than buffers; reassembling is what
    /// says splitting changed nothing about what is sent.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_build_context_goes_out_in_bounded_chunks() {
        let dir = a_context();
        std::fs::write(
            dir.path().join("big.bin"),
            vec![7u8; CONTEXT_CHUNK * 3 + 11],
        )
        .expect("writes");

        let dockerfile = Dockerfile::Inside("Dockerfile".into());
        let (chunks, reported) = streamed(dir.path(), &dockerfile).await;

        assert!(
            chunks.len() > 3,
            "the context went out in {} frame(s)",
            chunks.len()
        );
        for chunk in &chunks {
            assert!(
                chunk.len() <= CONTEXT_CHUNK,
                "a {}-byte frame went out whole",
                chunk.len()
            );
        }

        // Byte for byte what packing into memory produces.
        let streamed: Vec<u8> = chunks.concat();
        let whole = pack_context(dir.path(), &dockerfile, Vec::new()).expect("packs");
        assert_eq!(streamed, whole);
        assert!(matches!(reported, Packed::All), "the context did not pack");
    }

    /// Drains a context, keeping the events rather than the bytes.
    async fn events_of_streaming(context: &Path, dockerfile: &Dockerfile) -> Vec<String> {
        use futures::StreamExt;

        let (events, mut received) = EventSink::channel();
        let (mut chunks, packing) = stream_context(
            context,
            dockerfile,
            "building x",
            &events,
            &Remarked::default(),
        );

        while chunks.next().await.is_some() {}
        assert!(
            matches!(packing.await.expect("the packer ran"), Packed::All),
            "the context did not pack"
        );
        drop(events);

        let mut lines = Vec::new();
        while let Some(event) = received.recv().await {
            match event {
                kobune_api::Event::Step {
                    status: kobune_api::StepStatus::Progress { message },
                    ..
                }
                | kobune_api::Event::Log { message, .. } => lines.push(message),
                _ => {}
            }
        }

        lines
    }

    /// **A build that failed says why it failed.**
    ///
    /// The daemon answers a bad Dockerfile before it has read the context,
    /// so the packer's send has nowhere to go and it stops — which is a
    /// consequence of the failure, not the failure. Reported as one, every
    /// build that failed early over a context too large to have finished
    /// uploading would have said "the build stopped reading the context"
    /// instead of naming the step that died.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_request_that_stops_reading_is_not_the_reason_for_anything() {
        use futures::StreamExt;

        let dir = a_context();
        std::fs::write(
            dir.path().join("big.bin"),
            vec![7u8; CONTEXT_CHUNK * (CHUNKS_IN_FLIGHT + 4)],
        )
        .expect("writes");

        let (mut chunks, packing) = stream_context(
            dir.path(),
            &Dockerfile::Inside("Dockerfile".into()),
            "building x",
            &EventSink::discard(),
            &Remarked::default(),
        );

        // What a daemon that has made up its mind does: take a little and
        // go. The rest of the context has nowhere to be put.
        chunks
            .next()
            .await
            .expect("a first chunk")
            .expect("which is context, not an error");
        drop(chunks);

        assert!(
            matches!(packing.await.expect("the packer ran"), Packed::Cut),
            "a reader that went away was read as the context failing"
        );

        let mut none = Some(tokio::spawn(async { Packed::Cut }));
        assert_eq!(
            packing_failure(&mut none).await,
            None,
            "a cut-off body was offered as the reason a build failed"
        );
    }

    /// **A cut stays a cut through a layer that adds context.**
    ///
    /// `tar` hands a writer's error back untouched today, so comparing the
    /// message would do. It is one version away from not doing: a writer
    /// that named the file it was on would report a cancelled build as
    /// "packing src/main.rs: ..." instead, and the message check would
    /// miss — making [`Packed::Failed`] out of a cut and putting a
    /// sentence about a channel where the Dockerfile error belongs.
    #[test]
    fn a_cut_is_recognised_under_whatever_wrapped_it() {
        /// A wrapper of the shape that breaks a message comparison: it
        /// keeps the error underneath, and says something else itself.
        #[derive(Debug)]
        struct WhileWriting(&'static str, std::io::Error);

        impl std::fmt::Display for WhileWriting {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "packing {}: {}", self.0, self.1)
            }
        }

        impl std::error::Error for WhileWriting {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.1)
            }
        }

        let cut = || std::io::Error::new(std::io::ErrorKind::BrokenPipe, Cut);

        assert!(Cut::behind(&cut()), "a bare cut was not recognised");

        let named = std::io::Error::other(WhileWriting("src/main.rs", cut()));
        assert_ne!(
            named.get_ref().map(ToString::to_string),
            Some(Cut.to_string()),
            "the wrapper under test has to be one a message check would miss"
        );
        assert!(Cut::behind(&named), "a cut was lost behind added context");

        // And an `io::Error` inside an `io::Error`, which `source` reaches
        // past rather than returning.
        assert!(
            Cut::behind(&std::io::Error::other(cut())),
            "a doubly wrapped cut was not recognised"
        );

        // A file that genuinely could not be read is still a failure,
        // even spelled with the words a cut uses.
        for other in [
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
            std::io::Error::from(std::io::ErrorKind::BrokenPipe),
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, Cut.to_string()),
        ] {
            assert!(
                !Cut::behind(&other),
                "a file that could not be read was taken for a cut: {other}"
            );
        }
    }

    /// **How big the context is, said out loud.** `docker build` prints it;
    /// Kobune printed nothing, so a `.dockerignore` that had quietly stopped
    /// covering a directory looked like a bug in the build rather than 3 GB
    /// going over a socket.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_size_of_the_context_is_reported() {
        let dir = a_context();
        let lines = events_of_streaming(dir.path(), &Dockerfile::Inside("Dockerfile".into())).await;

        let said = lines
            .iter()
            .find(|line| line.starts_with("sending the build context:"))
            .unwrap_or_else(|| panic!("nothing said how much was sent: {lines:?}"));

        // The count is the walk's own, so what it says has to be what a
        // reader of the tar would find.
        let whole = pack_context(
            dir.path(),
            &Dockerfile::Inside("Dockerfile".into()),
            Vec::new(),
        )
        .expect("packs");
        assert_eq!(
            said,
            &format!(
                "sending the build context: {}",
                kobune_core::size::bytes(whole.len() as u64)
            )
        );
    }

    /// Packs past the threshold through one writer and hands back what it
    /// said. `remarked` is the flag the writer shares with the rest of its
    /// build — see [`Remarked`].
    ///
    /// Driven with zeroes rather than a fixture, because what is under test
    /// is the counting: half a gigabyte on disk would test the filesystem
    /// instead, and a sparse file — the cheap way to write one — is packed
    /// as a GNU sparse entry on Linux and as its zeroes on macOS, so it
    /// counts differently on the two.
    async fn what_a_walk_says(remarked: Remarked) -> Vec<String> {
        use futures::StreamExt;

        let (sender, receiver) = tokio::sync::mpsc::channel(CHUNKS_IN_FLIGHT);
        let (events, mut received) = EventSink::channel();

        let writing = tokio::task::spawn_blocking(move || {
            let mut writer = ChunkWriter::new(sender, events, "building x".to_string(), remarked);
            let block = [0u8; 64 * 1024];

            while writer.sent <= A_CONTEXT_WORTH_MENTIONING {
                std::io::Write::write_all(&mut writer, &block).expect("writes");
            }
        });

        let mut chunks = tokio_stream::wrappers::ReceiverStream::new(receiver);
        while chunks.next().await.is_some() {}
        writing.await.expect("wrote");

        let mut lines = Vec::new();
        while let Some(event) = received.recv().await {
            match event {
                kobune_api::Event::Step {
                    status: kobune_api::StepStatus::Progress { message },
                    ..
                }
                | kobune_api::Event::Log { message, .. } => lines.push(message),
                _ => {}
            }
        }

        lines
    }

    /// How many of `lines` called the context large.
    fn remarks(lines: &[String]) -> usize {
        lines
            .iter()
            .filter(|line| line.contains("the build context is over"))
            .count()
    }

    /// A context nobody meant to send is worth saying so about, even though
    /// it now works.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_context_nobody_meant_to_send_is_remarked_on() {
        let lines = what_a_walk_says(Remarked::default()).await;

        let remark = lines
            .iter()
            .find(|line| line.contains("the build context is over"))
            .unwrap_or_else(|| panic!("nothing remarked on it: {lines:?}"));
        assert!(
            remark.contains(".dockerignore"),
            "the remark does not say what to do about it: {remark}"
        );

        assert_eq!(remarks(&lines), 1, "said more than once");
    }

    /// **Twice packed is still one context.**
    ///
    /// A daemon that will not do a BuildKit build says so only once the
    /// request has gone, so [`DockerRuntime::ensure_built`] packs the same
    /// worktree a second time for the legacy builder. The remark is about
    /// the worktree, so a writer of its own per attempt would say it again
    /// — and being told twice reads as two problems.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_build_that_packs_its_context_twice_remarks_once() {
        let remarked = Remarked::default();

        let first = what_a_walk_says(Arc::clone(&remarked)).await;
        let second = what_a_walk_says(Arc::clone(&remarked)).await;

        assert_eq!(remarks(&first), 1, "the first walk did not remark on it");
        assert_eq!(
            remarks(&second),
            0,
            "the fallback said it again: {second:?}"
        );
    }

    /// A context that cannot be read says so, rather than leaving the
    /// daemon to build whatever arrived before the walk stopped.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_context_that_cannot_be_read_says_so() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("not-here");

        let dockerfile = Dockerfile::Inside("Dockerfile".into());
        let (_, reported) = streamed(&missing, &dockerfile).await;

        let Packed::Failed(err) = reported else {
            panic!("a context that is not there cannot be packed");
        };
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    /// The names in a packed context, sorted.
    fn packed(context: &Path, dockerfile: Dockerfile) -> Vec<String> {
        let tar = pack_context(context, &dockerfile, Vec::new()).expect("packs");

        let mut names: Vec<String> = tar::Archive::new(tar.as_slice())
            .entries()
            .expect("reads")
            .map(|entry| {
                entry
                    .expect("an entry")
                    .path()
                    .expect("a path")
                    .display()
                    .to_string()
            })
            .collect();

        names.sort();
        names
    }

    /// A context with a few files, a directory and a nested one.
    fn a_context() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();

        std::fs::write(
            root.join("Dockerfile"),
            "FROM alpine
",
        )
        .expect("writes");
        std::fs::write(root.join("package.json"), "{}").expect("writes");
        std::fs::write(root.join("debug.log"), "noise").expect("writes");
        std::fs::create_dir(root.join("src")).expect("creates");
        std::fs::write(root.join("src/main.rs"), "fn main() {}").expect("writes");
        std::fs::create_dir_all(root.join("node_modules/react")).expect("creates");
        std::fs::write(root.join("node_modules/react/index.js"), "//").expect("writes");

        dir
    }

    #[test]
    fn a_context_without_an_ignore_file_packs_what_it_always_did() {
        // **The one that matters most.** Walking the context by hand
        // replaced `append_dir_all`, and every build in the project goes
        // through it. This is what says the replacement is a replacement.
        let dir = a_context();

        let mut expected = {
            let mut builder = tar::Builder::new(Vec::new());
            builder.follow_symlinks(false);
            builder.append_dir_all(".", dir.path()).expect("packs");
            let tar = builder.into_inner().expect("finishes");

            tar::Archive::new(tar.as_slice())
                .entries()
                .expect("reads")
                .map(|entry| {
                    entry
                        .expect("an entry")
                        .path()
                        .expect("a path")
                        .display()
                        .to_string()
                })
                .collect::<Vec<_>>()
        };
        expected.sort();

        assert_eq!(
            packed(dir.path(), Dockerfile::Inside("Dockerfile".into())),
            expected
        );
    }

    #[test]
    fn a_dockerfile_from_outside_the_context_is_in_the_tar() {
        // **It was not, and nothing said so.** `into_inner` finishes the
        // archive, and the Dockerfile used to be appended to a second
        // builder wrapped around the finished bytes — past the two zero
        // blocks every reader stops at. The tar was the right length and
        // held nothing.
        let dir = a_context();
        let outside = tempfile::tempdir().expect("tempdir");
        let dockerfile = outside.path().join("web.Dockerfile");
        std::fs::write(&dockerfile, "FROM alpine\n").expect("writes");

        let names = packed(dir.path(), Dockerfile::Outside(dockerfile.clone()));

        assert!(
            names.contains(&DOCKERFILE_ENTRY.to_string()),
            "the Dockerfile is not there: {names:?}"
        );
        assert!(names.contains(&"src/main.rs".to_string()));
    }

    #[test]
    fn a_dockerfile_from_outside_survives_an_ignore_file_that_names_everything() {
        // It is added after the patterns have had their say, so there is
        // nothing for them to take out.
        let dir = a_context();
        std::fs::write(dir.path().join(".dockerignore"), "*\n").expect("writes");

        let outside = tempfile::tempdir().expect("tempdir");
        let dockerfile = outside.path().join("web.Dockerfile");
        std::fs::write(&dockerfile, "FROM alpine\n").expect("writes");

        let names = packed(dir.path(), Dockerfile::Outside(dockerfile.clone()));

        assert!(names.contains(&DOCKERFILE_ENTRY.to_string()));
        assert!(!names.contains(&"package.json".to_string()));
    }

    #[test]
    fn an_ignore_file_keeps_what_it_names_out_of_the_context() {
        let dir = a_context();
        std::fs::write(
            dir.path().join(".dockerignore"),
            "node_modules
*.log
",
        )
        .expect("writes");

        let names = packed(dir.path(), Dockerfile::Inside("Dockerfile".into()));

        assert!(!names.iter().any(|name| name.contains("node_modules")));
        assert!(!names.iter().any(|name| name.contains("debug.log")));
        assert!(names.contains(&"src/main.rs".to_string()));
        assert!(names.contains(&"Dockerfile".to_string()));
    }

    #[test]
    fn the_dockerfile_survives_an_ignore_file_that_names_everything() {
        // `*` with a few `!` lines is a common way to say "send almost
        // nothing", and it names the Dockerfile along with the rest. A
        // build that cannot find its own Dockerfile is no build.
        let dir = a_context();
        std::fs::write(
            dir.path().join(".dockerignore"),
            "*
!src
",
        )
        .expect("writes");

        let names = packed(dir.path(), Dockerfile::Inside("Dockerfile".into()));

        assert!(names.contains(&"Dockerfile".to_string()));
        assert!(names.contains(&"src/main.rs".to_string()));
        assert!(!names.contains(&"package.json".to_string()));
    }

    #[test]
    fn an_exception_reaches_inside_a_directory_that_was_left_out() {
        let dir = a_context();
        std::fs::write(
            dir.path().join(".dockerignore"),
            "node_modules\n!node_modules/react/index.js\n",
        )
        .expect("writes");

        let names = packed(dir.path(), Dockerfile::Inside("Dockerfile".into()));

        assert!(names.contains(&"node_modules/react/index.js".to_string()));
        // The directory itself stays out, as it does under `docker build`.
        assert!(!names.contains(&"node_modules".to_string()));
    }

    #[test]
    fn a_socket_in_the_context_does_not_take_the_build_down_with_it() {
        // A Rails `tmp/sockets` or a database left running in the worktree
        // used to fail the whole build: `tar` refuses to archive a socket.
        let dir = a_context();
        let socket = dir.path().join("app.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("binds");

        // What it used to do, pinned so the reason for skipping is not
        // rediscovered by someone tidying the walk.
        let mut builder = tar::Builder::new(Vec::new());
        builder.follow_symlinks(false);
        assert!(builder.append_dir_all(".", dir.path()).is_err());

        let names = packed(dir.path(), Dockerfile::Inside("Dockerfile".into()));

        assert!(!names.iter().any(|name| name.contains("app.sock")));
        assert!(names.contains(&"src/main.rs".to_string()));

        drop(listener);
    }
}
