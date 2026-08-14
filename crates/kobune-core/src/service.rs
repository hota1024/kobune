//! The runtime state of a service.
//!
//! The state machine is a concept, not an implementation, so it lives here
//! rather than in the runtime. Both `minato-api` and `minato-runtime`
//! share this type.

use serde::{Deserialize, Serialize};

/// The lifecycle of a single service.
///
/// ```text
/// Stopped ──(request arrives)──> Starting ──(health OK)──> Ready
///    ▲                              │                        │
///    └──(idle_timeout elapsed)─ Idle <┘ (health fails)        │
///                              ▲                             │
///                              └────(no traffic)─────────────┘
/// ```
///
/// `Idle` marks a service that is up but has not been touched for a while,
/// which is what makes it a candidate for scale-to-zero.
/// **On the wire it is a plain string**, never an object: `"ready"`,
/// `"failed"`. Every command speaks `--json` and Bash is meant to be
/// enough, so the most-read field on the most-run command has to be one
/// an agent can compare. It used to serialise as `{"state":"ready"}`,
/// which `.state == "ready"` never matches — found by running a real task
/// through the Skill (`docs/AGENT-RUN.md`).
///
/// **`reason` travels beside it, not inside it.** Whatever carries a
/// state carries an optional `reason` next to it — see
/// [`minato_api::ServiceInfo`]. That is the cost of the flat form, and it
/// is paid where the value is put together rather than by the reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceState {
    /// No container exists, or one was stopped on purpose.
    ///
    /// **Only when nothing went wrong.** A container that exited abnormally
    /// is [`Self::Failed`]; folding the two together leaves a start-up
    /// script that died looking like a service nobody started.
    Stopped,
    /// Starting up; the health check has not passed yet.
    Starting,
    /// Accepting requests.
    Ready,
    /// Running, but untouched for a while. A candidate for stopping.
    Idle,
    /// Failed to start, or exited abnormally.
    ///
    /// `reason` is shown to whoever asks, so it says what happened and
    /// where to look next.
    Failed { reason: String },
    /// The runtime could not be queried.
    Unknown,
}

impl Serialize for ServiceState {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.label())
    }
}

impl<'de> Deserialize<'de> for ServiceState {
    /// **`failed` comes back with an empty reason**, because the text is
    /// not in this field — it is beside it. Anything that needs it reads
    /// the `reason` next to the state, which is where it was written.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let label = String::deserialize(deserializer)?;

        Ok(match label.as_str() {
            "stopped" => Self::Stopped,
            "starting" => Self::Starting,
            "ready" => Self::Ready,
            "idle" => Self::Idle,
            "failed" => Self::failed(String::new()),
            "unknown" => Self::Unknown,
            other => {
                return Err(serde::de::Error::unknown_variant(
                    other,
                    &["stopped", "starting", "ready", "idle", "failed", "unknown"],
                ));
            }
        })
    }
}

impl ServiceState {
    /// The text behind a [`Self::Failed`], for putting beside the state
    /// on the wire.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Failed { reason } => Some(reason),
            _ => None,
        }
    }

    /// Whether a container exists for this service.
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Starting | Self::Ready | Self::Idle)
    }

    /// Whether requests may be forwarded here.
    pub fn is_serving(&self) -> bool {
        matches!(self, Self::Ready | Self::Idle)
    }

    pub fn failed(reason: impl Into<String>) -> Self {
        Self::Failed {
            reason: reason.into(),
        }
    }

    /// A short label for display.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Starting => "starting",
            Self::Ready => "ready",
            Self::Idle => "idle",
            Self::Failed { .. } => "failed",
            Self::Unknown => "unknown",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_states() {
        assert!(ServiceState::Ready.is_running());
        assert!(ServiceState::Ready.is_serving());

        assert!(ServiceState::Starting.is_running());
        assert!(
            !ServiceState::Starting.is_serving(),
            "do not forward while still starting"
        );

        assert!(
            ServiceState::Idle.is_serving(),
            "idle services are up, so they can serve"
        );

        assert!(!ServiceState::Stopped.is_running());
        assert!(!ServiceState::failed("boom").is_running());
        assert!(!ServiceState::Unknown.is_running());
    }

    #[test]
    fn serializes_as_a_plain_string() {
        // The point of the whole shape: an agent writes
        // `.state == "ready"` and it is true.
        assert_eq!(
            serde_json::to_string(&ServiceState::Ready).expect("serializes"),
            r#""ready""#
        );
        assert_eq!(
            serde_json::to_string(&ServiceState::failed("boom")).expect("serializes"),
            r#""failed""#,
            "the reason travels beside the state, not inside it"
        );
    }

    #[test]
    fn every_state_survives_the_round_trip() {
        for state in [
            ServiceState::Stopped,
            ServiceState::Starting,
            ServiceState::Ready,
            ServiceState::Idle,
            ServiceState::Unknown,
        ] {
            let json = serde_json::to_string(&state).expect("serializes");
            let back: ServiceState = serde_json::from_str(&json).expect("deserializes");
            assert_eq!(back, state);
        }
    }

    #[test]
    fn a_failure_comes_back_without_its_reason() {
        // Not an oversight — the text is not in this field. Pinned so
        // that anyone reaching for `state.reason()` after a round trip
        // finds this test rather than an empty string at run time.
        let json = serde_json::to_string(&ServiceState::failed("boom")).expect("serializes");
        let back: ServiceState = serde_json::from_str(&json).expect("deserializes");

        assert_eq!(back, ServiceState::failed(""));
        assert_eq!(
            back.reason(),
            Some(""),
            "read the `reason` beside the state instead"
        );
    }

    #[test]
    fn an_unknown_state_says_what_was_expected() {
        let err = serde_json::from_str::<ServiceState>(r#""dancing""#).unwrap_err();
        assert!(err.to_string().contains("dancing"), "got: {err}");
    }
}
