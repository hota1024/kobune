//! BuildKit's status stream, turned back into lines.
//!
//! The legacy builder reports a build as the text it would have printed:
//! one `stream` field per line, in order, and Kobune passes it straight
//! through. BuildKit reports a *graph* instead — vertexes that start,
//! finish, or turn out to be cached, with log output and byte counters
//! hanging off them — and says nothing about how to render it.
//!
//! [`Progress`] is that renderer. It follows `docker build`'s own plain
//! output closely enough to be recognised: each vertex gets a number when
//! it is first seen, and every line about it carries that number.
//!
//! **The number is the whole point.** BuildKit runs independent stages at
//! the same time, so two `RUN`s print into the same stream at once. Without
//! `#3` in front of a line there is no telling which stage said it, and a
//! failure's last few lines could belong to the stage that succeeded.

use std::collections::{HashMap, VecDeque};

use bollard::moby::buildkit::v1::StatusResponse;

/// How many of a step's own lines are kept for a failure to quote.
///
/// Matches what the legacy path keeps, so a build reads the same however it
/// was built.
const STEP_TAIL_LINES: usize = 12;

/// What has already been said about one vertex.
struct Vertex {
    /// The `#3` it is announced under.
    number: usize,
    /// Whether its name has been printed. A vertex is reported before it
    /// starts as well as after, and announcing it twice reads as two steps.
    announced: bool,
    /// Whether its outcome has been printed.
    ended: bool,
    /// The last few lines *this* vertex printed.
    ///
    /// **Per vertex rather than one tail for the build.** Stages run at the
    /// same time, so a shared tail of a dozen lines is a dozen lines of
    /// whichever stage talked last — and the one worth reading is the one
    /// that failed, which may have gone quiet several stages ago. See
    /// [`Progress::failure_tail`].
    recent: VecDeque<String>,
}

/// Turns [`StatusResponse`]s into the lines a build would have printed.
#[derive(Default)]
pub(crate) struct Progress {
    /// By vertex digest. BuildKit identifies everything else — a log line,
    /// a byte counter — by the digest of the vertex it belongs to.
    vertexes: HashMap<String, Vertex>,
    next_number: usize,
    /// The digest of the first vertex to report an error.
    ///
    /// What [`Progress::failure_tail`] reads, so a failure quotes the stage
    /// that actually broke rather than whichever one happened to print
    /// last. The first, because a failure takes the rest of the graph down
    /// with it and the ones cancelled by it have nothing to say.
    failed: Option<String>,
    /// How far each byte transfer has been reported, by vertex and id. See
    /// where it is read in [`Progress::absorb`].
    transfers: HashMap<(String, String), i64>,
}

impl Progress {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The lines this status response is worth, in the order to print them.
    ///
    /// **A step's output comes before its outcome.** BuildKit batches a
    /// vertex's last log lines into the same response as its completion, so
    /// walking the response field by field would print `#2 ERROR: …` ahead
    /// of the line saying why. Announcements, then output, then outcomes —
    /// which is the order `docker build` prints them in.
    pub(crate) fn absorb(&mut self, status: &StatusResponse) -> Vec<String> {
        let mut lines = Vec::new();

        for vertex in &status.vertexes {
            // A vertex Kobune has not announced is announced the moment
            // BuildKit starts it. The ones reported before that are the
            // graph being planned, and naming a step that may yet turn out
            // to be cached would be announcing work that never happens.
            if vertex.started.is_none() || self.entry(&vertex.digest).announced {
                continue;
            }

            let line = format!("#{} {}", self.number_of(&vertex.digest), vertex.name);
            let entry = self.entry(&vertex.digest);
            entry.announced = true;
            entry.remember(line.clone());
            lines.push(line);
        }

        for log in &status.logs {
            let number = self.number_of(&log.vertex);

            for line in String::from_utf8_lossy(&log.msg).lines() {
                let line = line.trim_end();
                if line.is_empty() {
                    continue;
                }

                let line = format!("#{number} {line}");
                self.entry(&log.vertex).remember(line.clone());
                lines.push(line);
            }
        }

        for vertex in &status.vertexes {
            if vertex.completed.is_none() || self.entry(&vertex.digest).ended {
                continue;
            }

            let number = self.number_of(&vertex.digest);
            self.entry(&vertex.digest).ended = true;

            if !vertex.error.is_empty() {
                self.failed.get_or_insert(vertex.digest.clone());
                lines.push(format!("#{number} ERROR: {}", vertex.error));
            } else if vertex.cached {
                lines.push(format!("#{number} CACHED"));
            } else {
                lines.push(format!("#{number} DONE"));
            }
        }

        for transfer in &status.statuses {
            // **Only transfers with a size to report.** BuildKit sends a
            // counter for every internal step it takes, most of them
            // without a total and without bytes — `exporting manifest`,
            // `naming to …`. Printing those buries the download that is
            // the reason for showing any of this at all.
            //
            // Tested before a number is handed out, so an internal step
            // never takes the number a real one would have been announced
            // under.
            if transfer.total <= 0 {
                continue;
            }

            // **A whole percent at a time.** BuildKit sends a counter for
            // every transfer in flight several times a second, and every
            // line becomes an event fanned out to every watching client.
            // A big base image would be thousands of them, all saying
            // almost the same thing. A hundred lines per transfer is
            // enough to watch a download move.
            let percent = (transfer.current * 100 / transfer.total).clamp(0, 100);
            let key = (transfer.vertex.clone(), transfer.id.clone());

            if self.transfers.insert(key, percent) == Some(percent) {
                continue;
            }

            lines.push(format!(
                "#{} {} {} / {}",
                self.number_of(&transfer.vertex),
                transfer.id,
                bytes(transfer.current),
                bytes(transfer.total),
            ));
        }

        lines
    }

    /// The last few lines the step that failed had printed.
    ///
    /// A step that printed nothing still has the line it was announced
    /// under, which names the command — `COPY` of a missing file dies
    /// without a word, and the command is the answer anyway.
    ///
    /// `None` when nothing has failed, and when what failed never started:
    /// there is nothing of its own to quote, and the caller has a
    /// build-wide tail to fall back on.
    pub(crate) fn failure_tail(&self) -> Option<Vec<String>> {
        let vertex = self.vertexes.get(self.failed.as_ref()?)?;

        match vertex.recent.is_empty() {
            true => None,
            false => Some(vertex.recent.iter().cloned().collect()),
        }
    }

    /// The number of the vertex with this digest, assigning one if it is new.
    fn number_of(&mut self, digest: &str) -> usize {
        self.entry(digest).number
    }

    fn entry(&mut self, digest: &str) -> &mut Vertex {
        if !self.vertexes.contains_key(digest) {
            self.next_number += 1;
            self.vertexes.insert(
                digest.to_string(),
                Vertex {
                    number: self.next_number,
                    announced: false,
                    ended: false,
                    recent: VecDeque::new(),
                },
            );
        }

        self.vertexes
            .get_mut(digest)
            .expect("inserted just above if it was missing")
    }
}

impl Vertex {
    /// Keeps a line this vertex printed, dropping the oldest past the cap.
    fn remember(&mut self, line: String) {
        if self.recent.len() == STEP_TAIL_LINES {
            self.recent.pop_front();
        }
        self.recent.push_back(line);
    }
}

/// [`kobune_core::size::bytes`] for a counter BuildKit keeps in `i64`.
///
/// Only the type is adapted; the rounding is shared with the CLI, which
/// prints these same counts back. A negative count is not a size — the
/// protobuf allows one, Docker does not send one — and reads as nothing
/// transferred rather than as an enormous number.
pub(crate) fn bytes(count: i64) -> String {
    kobune_core::size::bytes(count.max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::moby::buildkit::v1::{Vertex as PbVertex, VertexLog, VertexStatus};

    /// The rounding itself is [`kobune_core::size`]'s and tested there.
    /// What is this crate's is the `i64` a counter arrives as.
    #[test]
    fn a_counter_that_went_backwards_is_not_an_enormous_size() {
        assert_eq!(bytes(7_654_321), "7.2 MB");
        assert_eq!(bytes(0), "0 kB");
        assert_eq!(bytes(-1), "0 kB");
        assert_eq!(bytes(i64::MIN), "0 kB");
    }

    fn vertex(digest: &str, name: &str) -> PbVertex {
        PbVertex {
            digest: digest.to_string(),
            name: name.to_string(),
            ..Default::default()
        }
    }

    fn started(digest: &str, name: &str) -> PbVertex {
        PbVertex {
            started: Some(Default::default()),
            ..vertex(digest, name)
        }
    }

    fn finished(digest: &str, name: &str) -> PbVertex {
        PbVertex {
            completed: Some(Default::default()),
            ..started(digest, name)
        }
    }

    fn log(digest: &str, msg: &[u8]) -> StatusResponse {
        StatusResponse {
            logs: vec![VertexLog {
                vertex: digest.to_string(),
                msg: msg.to_vec(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn transfer(digest: &str, id: &str, current: i64, total: i64) -> StatusResponse {
        StatusResponse {
            statuses: vec![VertexStatus {
                vertex: digest.to_string(),
                id: id.to_string(),
                current,
                total,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn status(vertexes: Vec<PbVertex>) -> StatusResponse {
        StatusResponse {
            vertexes,
            ..Default::default()
        }
    }

    #[test]
    fn a_vertex_is_announced_when_it_starts_and_not_before() {
        let mut progress = Progress::new();

        // BuildKit reports the whole graph as it plans it. A step that may
        // still turn out to be cached is not work anyone did.
        assert!(
            progress
                .absorb(&status(vec![vertex("sha256:a", "[2/3] RUN npm ci")]))
                .is_empty()
        );

        assert_eq!(
            progress.absorb(&status(vec![started("sha256:a", "[2/3] RUN npm ci")])),
            vec!["#1 [2/3] RUN npm ci"]
        );
    }

    #[test]
    fn a_vertex_is_announced_once_however_often_it_is_reported() {
        let mut progress = Progress::new();
        let step = started("sha256:a", "[2/3] RUN npm ci");

        progress.absorb(&status(vec![step.clone()]));

        assert!(progress.absorb(&status(vec![step])).is_empty());
    }

    #[test]
    fn an_outcome_says_which_of_the_three_it_was() {
        let mut progress = Progress::new();
        progress.absorb(&status(vec![started("sha256:a", "one")]));
        progress.absorb(&status(vec![started("sha256:b", "two")]));
        progress.absorb(&status(vec![started("sha256:c", "three")]));

        let done = progress.absorb(&status(vec![finished("sha256:a", "one")]));
        let cached = progress.absorb(&status(vec![PbVertex {
            cached: true,
            ..finished("sha256:b", "two")
        }]));
        let failed = progress.absorb(&status(vec![PbVertex {
            error: "exit code: 1".to_string(),
            ..finished("sha256:c", "three")
        }]));

        assert_eq!(done, vec!["#1 DONE"]);
        assert_eq!(cached, vec!["#2 CACHED"]);
        assert_eq!(failed, vec!["#3 ERROR: exit code: 1"]);
    }

    #[test]
    fn an_outcome_is_reported_once() {
        let mut progress = Progress::new();
        progress.absorb(&status(vec![started("sha256:a", "one")]));
        progress.absorb(&status(vec![finished("sha256:a", "one")]));

        assert!(
            progress
                .absorb(&status(vec![finished("sha256:a", "one")]))
                .is_empty()
        );
    }

    #[test]
    fn output_carries_the_number_of_the_step_that_printed_it() {
        // The reason for numbering at all: BuildKit runs independent
        // stages together, so these two arrive interleaved.
        let mut progress = Progress::new();
        progress.absorb(&status(vec![started("sha256:a", "[web 2/3] RUN build")]));
        progress.absorb(&status(vec![started("sha256:b", "[api 2/3] RUN build")]));

        let lines = progress.absorb(&StatusResponse {
            logs: vec![
                VertexLog {
                    vertex: "sha256:b".to_string(),
                    msg: b"compiling\n".to_vec(),
                    ..Default::default()
                },
                VertexLog {
                    vertex: "sha256:a".to_string(),
                    msg: b"added 1 package\n".to_vec(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });

        assert_eq!(lines, vec!["#2 compiling", "#1 added 1 package"]);
    }

    #[test]
    fn one_log_message_may_hold_several_lines() {
        let mut progress = Progress::new();

        let lines = progress.absorb(&StatusResponse {
            logs: vec![VertexLog {
                vertex: "sha256:a".to_string(),
                msg: b"first\nsecond\n\n".to_vec(),
                ..Default::default()
            }],
            ..Default::default()
        });

        assert_eq!(lines, vec!["#1 first", "#1 second"]);
    }

    #[test]
    fn a_log_line_that_is_not_utf8_is_still_a_line() {
        let mut progress = Progress::new();

        let lines = progress.absorb(&StatusResponse {
            logs: vec![VertexLog {
                vertex: "sha256:a".to_string(),
                msg: vec![b'o', b'k', 0xff, b'\n'],
                ..Default::default()
            }],
            ..Default::default()
        });

        assert_eq!(lines, vec!["#1 ok\u{fffd}"]);
    }

    #[test]
    fn only_a_transfer_with_a_size_is_worth_a_line() {
        let mut progress = Progress::new();

        let lines = progress.absorb(&StatusResponse {
            statuses: vec![
                VertexStatus {
                    vertex: "sha256:a".to_string(),
                    id: "exporting layers".to_string(),
                    ..Default::default()
                },
                VertexStatus {
                    vertex: "sha256:a".to_string(),
                    id: "sha256:layer".to_string(),
                    current: 512 * 1024,
                    total: 8 * 1024 * 1024,
                    ..Default::default()
                },
            ],
            ..Default::default()
        });

        assert_eq!(lines, vec!["#1 sha256:layer 512 kB / 8.0 MB"]);
    }

    #[test]
    fn the_failing_step_is_the_first_one_to_report_an_error() {
        // A failure takes the rest of the graph down with it, and the
        // stage worth quoting is the one that broke, not the ones that
        // were cancelled because it did.
        let mut progress = Progress::new();
        progress.absorb(&status(vec![started("sha256:a", "one")]));
        progress.absorb(&status(vec![started("sha256:b", "two")]));
        progress.absorb(&log("sha256:a", b"still going\n"));
        progress.absorb(&log("sha256:b", b"the real reason\n"));

        assert_eq!(progress.failure_tail(), None);

        progress.absorb(&status(vec![PbVertex {
            error: "exit code: 1".to_string(),
            ..finished("sha256:b", "two")
        }]));
        progress.absorb(&status(vec![PbVertex {
            error: "context canceled".to_string(),
            ..finished("sha256:a", "one")
        }]));

        assert_eq!(
            progress.failure_tail(),
            Some(vec!["#2 two".to_string(), "#2 the real reason".to_string()])
        );
    }

    #[test]
    fn a_step_that_printed_nothing_is_still_quoted_by_name() {
        // `COPY` of a missing file dies without a word, and the one line
        // worth having is the command itself — which is the line the step
        // was announced under.
        let mut progress = Progress::new();
        progress.absorb(&status(vec![PbVertex {
            error: "not found".to_string(),
            ..finished("sha256:a", "[1/2] COPY missing .")
        }]));

        assert_eq!(
            progress.failure_tail(),
            Some(vec!["#1 [1/2] COPY missing .".to_string()])
        );
    }

    #[test]
    fn a_step_that_never_started_leaves_the_tail_to_the_caller() {
        // Nothing was announced and nothing printed, so there is nothing
        // of this step's to quote and the build-wide tail is all there is.
        let mut progress = Progress::new();
        progress.absorb(&status(vec![PbVertex {
            error: "failed to solve".to_string(),
            completed: Some(Default::default()),
            ..vertex("sha256:a", "[1/2] RUN build")
        }]));

        assert_eq!(progress.failure_tail(), None);
    }

    #[test]
    fn a_step_keeps_only_its_last_lines() {
        let mut progress = Progress::new();
        progress.absorb(&status(vec![started("sha256:a", "one")]));

        for line in 0..40 {
            progress.absorb(&log("sha256:a", format!("line {line}\n").as_bytes()));
        }
        progress.absorb(&status(vec![PbVertex {
            error: "exit code: 1".to_string(),
            ..finished("sha256:a", "one")
        }]));

        let tail = progress.failure_tail().expect("a tail");

        assert_eq!(tail.len(), STEP_TAIL_LINES);
        assert_eq!(tail.last().unwrap(), "#1 line 39");
    }

    #[test]
    fn a_steps_output_comes_before_its_outcome() {
        // BuildKit batches a vertex's last lines with its completion, so
        // walking the response field by field would print the error ahead
        // of the line saying why.
        let mut progress = Progress::new();

        let lines = progress.absorb(&StatusResponse {
            vertexes: vec![PbVertex {
                error: "exit code: 1".to_string(),
                ..finished("sha256:a", "[2/2] RUN build")
            }],
            logs: vec![VertexLog {
                vertex: "sha256:a".to_string(),
                msg: b"the real reason\n".to_vec(),
                ..Default::default()
            }],
            ..Default::default()
        });

        assert_eq!(
            lines,
            vec![
                "#1 [2/2] RUN build",
                "#1 the real reason",
                "#1 ERROR: exit code: 1",
            ]
        );
    }

    #[test]
    fn a_transfer_is_reported_a_whole_percent_at_a_time() {
        // BuildKit sends a counter several times a second per transfer in
        // flight, and every line becomes an event for every watching
        // client. A base image would otherwise be thousands of them.
        let mut progress = Progress::new();

        let mut lines = 0;
        for current in 0..=1000 {
            lines += progress
                .absorb(&transfer(
                    "sha256:a",
                    "sha256:layer",
                    current * 1024,
                    1000 * 1024,
                ))
                .len();
        }

        assert_eq!(lines, 101, "one line per percent, and no more");
    }

    #[test]
    fn an_internal_counter_takes_no_step_number() {
        // `exporting manifest` and friends carry no size and are not
        // shown, so they must not shift the numbering of what is.
        let mut progress = Progress::new();
        progress.absorb(&transfer("sha256:internal", "exporting manifest", 0, 0));

        assert_eq!(
            progress.absorb(&status(vec![started("sha256:a", "[1/1] RUN build")])),
            vec!["#1 [1/1] RUN build"]
        );
    }
}
