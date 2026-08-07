//! Events the daemon emits while working.
//!
//! A request yields zero or more events and then exactly one response. The
//! CLI turns them into a spinner and printed lines; the GUI into a progress
//! bar and a log pane. **Both must be derivable from the same event
//! stream** — that is a design requirement (`docs/DESIGN.md` §3).

use minato_core::ServiceState;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// A line describing what is happening.
    Log { level: LogLevel, message: String },

    /// Progress of a named step.
    ///
    /// `id` is a stable identifier the GUI uses to follow updates to the
    /// same step; `label` is the text to show.
    Step {
        id: String,
        label: String,
        #[serde(flatten)]
        status: StepStatus,
    },

    /// A service changed state.
    ServiceState {
        service: String,
        state: ServiceState,
    },

    /// Container or build output, passed through verbatim.
    Output {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        service: Option<String>,
        stream: OutputStream,
        line: String,
    },
}

impl Event {
    pub fn log(level: LogLevel, message: impl Into<String>) -> Self {
        Self::Log {
            level,
            message: message.into(),
        }
    }

    pub fn info(message: impl Into<String>) -> Self {
        Self::log(LogLevel::Info, message)
    }

    pub fn warn(message: impl Into<String>) -> Self {
        Self::log(LogLevel::Warn, message)
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::log(LogLevel::Error, message)
    }

    pub fn step_started(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self::Step {
            id: id.into(),
            label: label.into(),
            status: StepStatus::Started,
        }
    }

    pub fn step_done(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self::Step {
            id: id.into(),
            label: label.into(),
            status: StepStatus::Done,
        }
    }

    pub fn step_failed(
        id: impl Into<String>,
        label: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self::Step {
            id: id.into(),
            label: label.into(),
            status: StepStatus::Failed {
                reason: reason.into(),
            },
        }
    }

    pub fn step_skipped(
        id: impl Into<String>,
        label: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self::Step {
            id: id.into(),
            label: label.into(),
            status: StepStatus::Skipped {
                reason: reason.into(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum StepStatus {
    Started,
    /// Incidental detail while running: build stages, bytes downloaded.
    Progress {
        message: String,
    },
    Done,
    Failed {
        reason: String,
    },
    /// Nothing to do — an already-running service, for instance.
    Skipped {
        reason: String,
    },
}

impl StepStatus {
    /// Whether this step will receive no further updates.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Done | Self::Failed { .. } | Self::Skipped { .. }
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputStream {
    Stdout,
    Stderr,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_flattens_status_onto_the_event() {
        let event = Event::step_started("prepare-network", "creating the network");
        let json = serde_json::to_string(&event).expect("serializes");

        // Keeping status flat rather than nested simplifies the GUI's match.
        assert!(json.contains(r#""kind":"step""#), "got: {json}");
        assert!(json.contains(r#""status":"started""#), "got: {json}");
        assert!(json.contains(r#""id":"prepare-network""#), "got: {json}");
    }

    #[test]
    fn roundtrips_every_variant() {
        let events = vec![
            Event::info("starting up"),
            Event::warn("the image may be out of date"),
            Event::error("cannot connect"),
            Event::step_started("pull", "pulling the image"),
            Event::step_done("pull", "pulling the image"),
            Event::step_failed("start", "starting the container", "port in use"),
            Event::step_skipped("pull", "pulling the image", "already present"),
            Event::Step {
                id: "build".into(),
                label: "building".into(),
                status: StepStatus::Progress {
                    message: "3/8".into(),
                },
            },
            Event::ServiceState {
                service: "web".into(),
                state: ServiceState::Ready,
            },
            Event::Output {
                service: Some("web".into()),
                stream: OutputStream::Stdout,
                line: "listening on 3000".into(),
            },
        ];

        for event in events {
            let json = serde_json::to_string(&event).expect("serializes");
            let back: Event = serde_json::from_str(&json).expect("deserializes");
            assert_eq!(back, event, "json = {json}");
        }
    }

    #[test]
    fn identifies_terminal_steps() {
        assert!(!StepStatus::Started.is_terminal());
        assert!(
            !StepStatus::Progress {
                message: "x".into()
            }
            .is_terminal()
        );
        assert!(StepStatus::Done.is_terminal());
        assert!(StepStatus::Failed { reason: "x".into() }.is_terminal());
        assert!(StepStatus::Skipped { reason: "x".into() }.is_terminal());
    }

    #[test]
    fn log_levels_are_ordered() {
        assert!(LogLevel::Debug < LogLevel::Info);
        assert!(LogLevel::Warn < LogLevel::Error);
    }
}
