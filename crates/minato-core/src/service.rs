//! サービスの実行状態。
//!
//! 状態機械の定義は概念であって実装ではないため、runtime ではなくここに置く。
//! `minato-api` と `minato-runtime` の両方がこの型を共有する。

use serde::{Deserialize, Serialize};

/// サービス 1 つのライフサイクル。
///
/// ```text
/// Stopped ──(リクエスト到達)──> Starting ──(health OK)──> Ready
///    ▲                             │                        │
///    └──(idle_timeout 経過)── Idle <┘ (health NG)            │
///                              ▲                            │
///                              └────(無アクセス継続)─────────┘
/// ```
///
/// `Idle` は M2（scale-to-zero）で使い始める。M0 では `Stopped` / `Starting` /
/// `Ready` / `Failed` のみが現れる。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ServiceState {
    /// コンテナが存在しない、または停止している。
    Stopped,
    /// 起動処理中。health check がまだ通っていない。
    Starting,
    /// 受け付け可能。
    Ready,
    /// 起動しているが一定時間アクセスがない。停止候補。
    Idle,
    /// 起動に失敗した、または異常終了した。
    Failed { reason: String },
    /// runtime に問い合わせられず判定できない。
    Unknown,
}

impl ServiceState {
    /// コンテナが存在している状態か。
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Starting | Self::Ready | Self::Idle)
    }

    /// リクエストを転送してよい状態か。
    pub fn is_serving(&self) -> bool {
        matches!(self, Self::Ready | Self::Idle)
    }

    pub fn failed(reason: impl Into<String>) -> Self {
        Self::Failed {
            reason: reason.into(),
        }
    }

    /// 表示用の短いラベル。
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
        assert!(!ServiceState::Starting.is_serving(), "起動中には転送しない");

        assert!(
            ServiceState::Idle.is_serving(),
            "idle は起動済みなので転送できる"
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
