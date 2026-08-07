//! runtime から呼び出し元へ進捗を返す口。
//!
//! runtime は自分がどこに繋がれているか（CLI か GUI か、そもそも誰も
//! 見ていないか）を知らない。イベントを投げるだけにする。

use minato_api::{Event, LogLevel, OutputStream, StepStatus};
use minato_core::ServiceState;
use tokio::sync::mpsc;

/// イベントの送り先。
///
/// 送信は同期・ノンブロッキングにする。runtime の処理速度が
/// 受け手の描画速度に引きずられてはいけないため。
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

    /// どこにも送らない sink。テストや、進捗を必要としない内部呼び出しで使う。
    pub fn discard() -> Self {
        Self::default()
    }

    /// 送信用のチャネルと、それを読む受け手を作る。
    pub fn channel() -> (Self, mpsc::UnboundedReceiver<Event>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self::new(tx), rx)
    }

    /// 受け手が既に落ちていても失敗にはしない。
    /// 進捗が届かないことは処理の失敗ではない。
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

    pub fn output(&self, service: Option<String>, stream: OutputStream, line: impl Into<String>) {
        self.send(Event::Output {
            service,
            stream,
            line: line.into(),
        });
    }

    /// ステップの開始と終了で処理を挟む。
    ///
    /// 失敗時には自動で `Failed` を送るため、呼び出し側が
    /// エラーパスで送り忘れることがない。
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
        sink.info("何も起きない");
        sink.step_started("x", "y");
        sink.service_state("web", ServiceState::Ready);
    }

    #[tokio::test]
    async fn delivers_events_in_order() {
        let (sink, mut rx) = EventSink::channel();

        sink.step_started("pull", "取得");
        sink.info("進行中");
        sink.step_done("pull", "取得");
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

        // 受け手が消えても runtime の処理は続行できなければならない。
        sink.info("誰も聞いていない");
    }

    #[tokio::test]
    async fn track_emits_done_on_success() {
        let (sink, mut rx) = EventSink::channel();

        let result: Result<u8, String> = sink.track("step", "ラベル", async { Ok(42) }).await;
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
            .track("step", "ラベル", async {
                Err("失敗しました".to_string())
            })
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
                    reason: "失敗しました".into()
                }
            ]
        );
    }
}
