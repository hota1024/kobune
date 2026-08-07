//! 処理中に daemon が送出するイベント。
//!
//! 1 リクエストに対して 0 個以上のイベントが流れ、最後に 1 つの応答が返る。
//! CLI はこれをスピナーと行出力に、GUI は進捗バーとログペインに変換する。
//! **同じイベント列から両方が作れること**が設計上の要件（`docs/DESIGN.md` §3）。

use minato_core::ServiceState;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// 状況を伝えるログ行。
    Log { level: LogLevel, message: String },

    /// 名前付きステップの進捗。
    ///
    /// `id` は安定した識別子で、GUI が同じステップの更新を追跡するのに使う。
    /// `label` は表示用の文言。
    Step {
        id: String,
        label: String,
        #[serde(flatten)]
        status: StepStatus,
    },

    /// サービスの状態遷移。
    ServiceState {
        service: String,
        state: ServiceState,
    },

    /// コンテナやビルドの出力をそのまま転送する。
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
    /// 進行中の付随情報（ビルドの段数、ダウンロード量など）。
    Progress {
        message: String,
    },
    Done,
    Failed {
        reason: String,
    },
    /// 実行不要だった場合。既に起動済みのサービスなど。
    Skipped {
        reason: String,
    },
}

impl StepStatus {
    /// このステップがこれ以上更新されないか。
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
        let event = Event::step_started("prepare-network", "ネットワークを作成");
        let json = serde_json::to_string(&event).expect("serializes");

        // status がネストせず同じ階層に出ることで、GUI 側の分岐が単純になる。
        assert!(json.contains(r#""kind":"step""#), "got: {json}");
        assert!(json.contains(r#""status":"started""#), "got: {json}");
        assert!(json.contains(r#""id":"prepare-network""#), "got: {json}");
    }

    #[test]
    fn roundtrips_every_variant() {
        let events = vec![
            Event::info("起動しています"),
            Event::warn("イメージが古い可能性があります"),
            Event::error("接続できません"),
            Event::step_started("pull", "イメージを取得"),
            Event::step_done("pull", "イメージを取得"),
            Event::step_failed("start", "コンテナを起動", "port in use"),
            Event::step_skipped("pull", "イメージを取得", "既に存在します"),
            Event::Step {
                id: "build".into(),
                label: "ビルド".into(),
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
