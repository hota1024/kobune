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

use std::collections::HashMap;

use bollard::moby::buildkit::v1::StatusResponse;

/// What has already been said about one vertex.
struct Vertex {
    /// The `#3` it is announced under.
    number: usize,
    /// Whether its name has been printed. A vertex is reported before it
    /// starts as well as after, and announcing it twice reads as two steps.
    announced: bool,
    /// Whether its outcome has been printed.
    ended: bool,
}

/// Turns [`StatusResponse`]s into the lines a build would have printed.
#[derive(Default)]
pub(crate) struct Progress {
    /// By vertex digest. BuildKit identifies everything else — a log line,
    /// a byte counter — by the digest of the vertex it belongs to.
    vertexes: HashMap<String, Vertex>,
    next_number: usize,
    /// The number of the first vertex to report an error.
    ///
    /// What [`Progress::failed`] hands to the failure message so it can
    /// quote the stage that actually broke rather than whichever one
    /// happened to print last.
    failed: Option<usize>,
}

impl Progress {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The lines this status response is worth, in the order to print them.
    pub(crate) fn absorb(&mut self, status: &StatusResponse) -> Vec<String> {
        let mut lines = Vec::new();

        for vertex in &status.vertexes {
            let number = self.number_of(&vertex.digest);

            // A vertex Kobune has not announced is announced the moment
            // BuildKit starts it. The ones reported before that are the
            // graph being planned, and naming a step that may yet turn out
            // to be cached would be announcing work that never happens.
            if vertex.started.is_some() && !self.entry(&vertex.digest).announced {
                self.entry(&vertex.digest).announced = true;
                lines.push(format!("#{number} {}", vertex.name));
            }

            if vertex.completed.is_none() || self.entry(&vertex.digest).ended {
                continue;
            }
            self.entry(&vertex.digest).ended = true;

            if !vertex.error.is_empty() {
                self.failed.get_or_insert(number);
                lines.push(format!("#{number} ERROR: {}", vertex.error));
            } else if vertex.cached {
                lines.push(format!("#{number} CACHED"));
            } else {
                lines.push(format!("#{number} DONE"));
            }
        }

        for log in &status.logs {
            let number = self.number_of(&log.vertex);
            for line in String::from_utf8_lossy(&log.msg).lines() {
                let line = line.trim_end();
                if !line.is_empty() {
                    lines.push(format!("#{number} {line}"));
                }
            }
        }

        for transfer in &status.statuses {
            let number = self.number_of(&transfer.vertex);

            // **Only transfers with a size to report.** BuildKit sends a
            // counter for every internal step it takes, most of them
            // without a total and without bytes — `exporting manifest`,
            // `naming to …`. Printing those buries the download that is
            // the reason for showing any of this at all.
            if transfer.total <= 0 {
                continue;
            }

            lines.push(format!(
                "#{number} {} {} / {}",
                transfer.id,
                bytes(transfer.current),
                bytes(transfer.total),
            ));
        }

        lines
    }

    /// The prefix the failing vertex's lines carry, once one has failed.
    pub(crate) fn failed(&self) -> Option<String> {
        self.failed.map(|number| format!("#{number} "))
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
                },
            );
        }

        self.vertexes
            .get_mut(digest)
            .expect("inserted just above if it was missing")
    }
}

/// A byte count, short enough to sit on a progress line.
///
/// Its own rather than the CLI's: `kobune-runtime` is on the daemon side of
/// the API and no client crate may reach it, so the two cannot share one.
/// The rounding matches, so the two read alike.
fn bytes(count: i64) -> String {
    const KB: i64 = 1024;
    const MB: i64 = 1024 * KB;

    if count >= MB {
        format!("{}.{} MB", count / MB, (count % MB) * 10 / MB)
    } else {
        format!("{} kB", count / KB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::moby::buildkit::v1::{Vertex as PbVertex, VertexLog, VertexStatus};

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

        assert_eq!(progress.failed(), None);

        progress.absorb(&status(vec![PbVertex {
            error: "exit code: 1".to_string(),
            ..finished("sha256:b", "two")
        }]));
        progress.absorb(&status(vec![PbVertex {
            error: "context canceled".to_string(),
            ..finished("sha256:a", "one")
        }]));

        assert_eq!(progress.failed().as_deref(), Some("#2 "));
    }
}
