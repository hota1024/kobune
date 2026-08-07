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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ServiceState {
    /// No container exists, or it is stopped.
    Stopped,
    /// Starting up; the health check has not passed yet.
    Starting,
    /// Accepting requests.
    Ready,
    /// Running, but untouched for a while. A candidate for stopping.
    Idle,
    /// Failed to start, or exited abnormally.
    Failed { reason: String },
    /// The runtime could not be queried.
    Unknown,
}

impl ServiceState {
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
    fn serializes_with_tag() {
        let json = serde_json::to_string(&ServiceState::Ready).expect("serializes");
        assert_eq!(json, r#"{"state":"ready"}"#);

        let json = serde_json::to_string(&ServiceState::failed("boom")).expect("serializes");
        assert_eq!(json, r#"{"state":"failed","reason":"boom"}"#);

        let back: ServiceState = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back, ServiceState::failed("boom"));
    }
}
