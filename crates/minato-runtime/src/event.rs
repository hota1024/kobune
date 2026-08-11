//! How a runtime reports progress back to its caller.
//!
//! A runtime does not know what it is attached to — a CLI, a GUI, or
//! nobody at all. All it does is emit events.

use minato_api::{Event, LogLevel, OutputStream, StepStatus};
use minato_core::ServiceState;
use tokio::sync::mpsc;

/// Where events go.
///
/// Sending is synchronous and non-blocking: how fast the runtime works
/// must not be dragged down by how fast the receiver draws.
#[derive(Clone, Debug, Default)]
pub struct EventSink {
    sender: Option<mpsc::UnboundedSender<Event>>,
}

impl EventSink {
    pub fn new(sender: mpsc::UnboundedSender<Event>) -> Self {
        Self {
            sender: Some(sender),
        }
    }

    /// A sink that goes nowhere. For tests, and for internal calls that
    /// have no use for progress.
    pub fn discard() -> Self {
        Self::default()
    }

    /// Builds a sink and the receiver that reads from it.
    pub fn channel() -> (Self, mpsc::UnboundedReceiver<Event>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self::new(tx), rx)
    }

    /// A receiver that has already gone away is not a failure. Progress
    /// not arriving is not the work failing.
    pub fn send(&self, event: Event) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(event);
        }
    }

    pub fn log(&self, level: LogLevel, message: impl Into<String>) {
        self.send(Event::log(level, message));
    }

    pub fn debug(&self, message: impl Into<String>) {
        self.log(LogLevel::Debug, message);
    }

    pub fn info(&self, message: impl Into<String>) {
        self.log(LogLevel::Info, message);
    }

    pub fn warn(&self, message: impl Into<String>) {
        self.log(LogLevel::Warn, message);
    }

    pub fn error(&self, message: impl Into<String>) {
        self.log(LogLevel::Error, message);
    }

    pub fn step_started(&self, id: impl Into<String>, label: impl Into<String>) {
        self.send(Event::step_started(id, label));
    }

    pub fn step_done(&self, id: impl Into<String>, label: impl Into<String>) {
        self.send(Event::step_done(id, label));
    }

    pub fn step_failed(
        &self,
        id: impl Into<String>,
        label: impl Into<String>,
        reason: impl Into<String>,
    ) {
        self.send(Event::step_failed(id, label, reason));
    }

    pub fn step_skipped(
        &self,
        id: impl Into<String>,
        label: impl Into<String>,
        reason: impl Into<String>,
    ) {
        self.send(Event::step_skipped(id, label, reason));
    }

    pub fn step_progress(
        &self,
        id: impl Into<String>,
        label: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.send(Event::Step {
            id: id.into(),
            label: label.into(),
            status: StepStatus::Progress {
                message: message.into(),
            },
        });
    }

    pub fn service_state(&self, service: impl Into<String>, state: ServiceState) {
        self.send(Event::ServiceState {
            service: service.into(),
            state,
        });
    }

    /// Says that the client's terminal now belongs to a service.
    pub fn attached(&self, service: impl Into<String>) {
        self.send(Event::Attached {
            service: service.into(),
        });
    }

    /// Passes terminal output along, byte for byte.
    pub fn bytes(&self, service: Option<String>, bytes: &[u8]) {
        self.send(Event::bytes(service, bytes));
    }

    pub fn output(&self, service: Option<String>, stream: OutputStream, line: impl Into<String>) {
        self.send(Event::Output {
            service,
            stream,
            line: line.into(),
        });
    }

    /// Brackets a piece of work with a step start and finish.
    ///
    /// `Failed` goes out automatically, so no caller can forget it on the
    /// error path.
    pub async fn track<T, E, F>(
        &self,
        id: &str,
        label: &str,
        future: F,
    ) -> std::result::Result<T, E>
    where
        F: std::future::Future<Output = std::result::Result<T, E>>,
        E: std::fmt::Display,
    {
        self.step_started(id, label);
        match future.await {
            Ok(value) => {
                self.step_done(id, label);
                Ok(value)
            }
            Err(err) => {
                self.step_failed(id, label, err.to_string());
                Err(err)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn discard_sink_accepts_everything() {
        let sink = EventSink::discard();
        sink.info("nothing happens");
        sink.step_started("x", "y");
        sink.service_state("web", ServiceState::Ready);
    }

    #[tokio::test]
    async fn delivers_events_in_order() {
        let (sink, mut rx) = EventSink::channel();

        sink.step_started("pull", "pulling");
        sink.info("in progress");
        sink.step_done("pull", "pulling");
        drop(sink);

        let mut received = Vec::new();
        while let Some(event) = rx.recv().await {
            received.push(event);
        }

        assert_eq!(received.len(), 3);
        assert!(matches!(received[0], Event::Step { .. }));
        assert!(matches!(received[1], Event::Log { .. }));
    }

    #[tokio::test]
    async fn sending_after_receiver_drop_is_not_an_error() {
        let (sink, rx) = EventSink::channel();
        drop(rx);

        // The runtime has to carry on after its receiver disappears.
        sink.info("nobody is listening");
    }

    #[tokio::test]
    async fn track_emits_done_on_success() {
        let (sink, mut rx) = EventSink::channel();

        let result: Result<u8, String> = sink.track("step", "label", async { Ok(42) }).await;
        assert_eq!(result, Ok(42));
        drop(sink);

        let mut statuses = Vec::new();
        while let Some(Event::Step { status, .. }) = rx.recv().await {
            statuses.push(status);
        }
        assert_eq!(statuses, vec![StepStatus::Started, StepStatus::Done]);
    }

    #[tokio::test]
    async fn track_emits_failure_without_caller_involvement() {
        let (sink, mut rx) = EventSink::channel();

        let result: Result<(), String> = sink
            .track("step", "label", async { Err("it failed".to_string()) })
            .await;
        assert!(result.is_err());
        drop(sink);

        let mut statuses = Vec::new();
        while let Some(Event::Step { status, .. }) = rx.recv().await {
            statuses.push(status);
        }

        assert_eq!(
            statuses,
            vec![
                StepStatus::Started,
                StepStatus::Failed {
                    reason: "it failed".into()
                }
            ]
        );
    }
}
