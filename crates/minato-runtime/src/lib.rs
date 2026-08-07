//! 仮想化バックエンドの抽象と実装。
//!
//! `Runtime` trait が返す `endpoint` はプロキシの転送先であり、それが
//! ホストのフォワードポートかコンテナ自身の IP かは実装が決める。
//! この 1 点で Docker と Apple Container の構造的な違いを吸収している。

pub mod apple;
pub mod docker;
pub mod error;
pub mod event;
pub mod health;
pub mod runtime;
pub mod spec;

pub use apple::AppleContainerRuntime;
pub use docker::DockerRuntime;
pub use error::{Result, RuntimeError};
pub use event::EventSink;
pub use health::{DEFAULT_READINESS_TIMEOUT, await_service, probe, wait_until_ready};
pub use runtime::{ExecOutcome, LogLine, LogOptions, Runtime, RuntimeInfo, labels, names};
pub use spec::{
    RunningService, ServiceKey, ServiceSpec, ServiceStatus, SourceMount, VolumeMount, WorkspaceKey,
    WorkspaceSpec,
};

/// 識別子から runtime 実装を作る。
///
/// 接続確認はしないため、ここで成功しても使えるとは限らない。
/// 呼び出し側は [`Runtime::probe`] で到達性を確かめる。
pub fn create(id: &str) -> Result<Box<dyn Runtime>> {
    match id {
        "docker" => Ok(Box::new(DockerRuntime::connect()?)),
        "apple" | "apple-container" | "container" => Ok(Box::new(AppleContainerRuntime::new())),
        other => Err(RuntimeError::Unsupported(format!(
            "runtime `{other}` は未対応です。`docker` または `apple` を指定してください"
        ))),
    }
}

/// 対応している runtime 識別子。
pub const AVAILABLE_RUNTIMES: &[&str] = &["docker", "apple"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_runtime_with_a_useful_message() {
        // `Box<dyn Runtime>` は Debug を実装しないので unwrap_err は使えない。
        let message = match create("podman") {
            Ok(runtime) => panic!("未対応のはずが {} を返した", runtime.id()),
            Err(err) => err.to_string(),
        };

        assert!(message.contains("podman"), "何が悪いか: {message}");
        assert!(message.contains("docker"), "何が使えるか: {message}");
    }

    #[test]
    fn accepts_apple_container_aliases() {
        for id in ["apple", "apple-container", "container"] {
            let runtime = create(id).expect("作れる");
            assert_eq!(runtime.id(), "apple");
        }
    }
}
