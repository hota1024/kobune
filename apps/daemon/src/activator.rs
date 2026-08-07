//! プロキシからの起動要求を Supervisor に繋ぐ。
//!
//! Gateway（プロキシ）は起動時に Activator を必要とし、Supervisor は
//! URL の発行に Gateway を必要とする。この循環を、後から実体を差し込む
//! [`DeferredActivator`] で解く。

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use minato_proxy::{Activation, Activator};

use crate::supervisor::Supervisor;

/// Supervisor を Activator として見せる。
///
/// `impl Activator for Arc<Supervisor>` は孤児ルールで書けないため
/// newtype を挟む。
pub struct SupervisorActivator(Arc<Supervisor>);

impl SupervisorActivator {
    pub fn new(supervisor: Arc<Supervisor>) -> Self {
        Self(supervisor)
    }
}

#[async_trait]
impl Activator for SupervisorActivator {
    async fn ensure_ready(&self, host: &str, wait: Duration) -> Activation {
        self.0.activate(host, wait).await
    }

    fn touch(&self, host: &str) {
        self.0.touch(host);
    }
}

/// 実体が後から差し込まれる Activator。
///
/// 差し込まれる前に呼ばれた場合は「知らないホスト」として扱う。
/// daemon の起動直後の極めて短い間だけ起こりうる。
#[derive(Clone, Default)]
pub struct DeferredActivator {
    inner: Arc<OnceLock<Arc<dyn Activator>>>,
}

impl DeferredActivator {
    pub fn new() -> Self {
        Self::default()
    }

    /// 実体を差し込む。2 回目以降は無視される。
    pub fn set(&self, activator: Arc<dyn Activator>) {
        if self.inner.set(activator).is_err() {
            tracing::warn!("Activator は既に設定されています");
        }
    }
}

#[async_trait]
impl Activator for DeferredActivator {
    async fn ensure_ready(&self, host: &str, wait: Duration) -> Activation {
        match self.inner.get() {
            Some(activator) => activator.ensure_ready(host, wait).await,
            None => {
                tracing::debug!("Activator がまだ設定されていません: {host}");
                Activation::Unknown
            }
        }
    }

    fn touch(&self, host: &str) {
        if let Some(activator) = self.inner.get() {
            activator.touch(host);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counting(AtomicUsize);

    #[async_trait]
    impl Activator for Counting {
        async fn ensure_ready(&self, _host: &str, _wait: Duration) -> Activation {
            self.0.fetch_add(1, Ordering::SeqCst);
            Activation::Ready(SocketAddr::from(([127, 0, 0, 1], 3000)))
        }

        fn touch(&self, _host: &str) {}
    }

    #[tokio::test]
    async fn unset_activator_reports_unknown() {
        let deferred = DeferredActivator::new();

        assert_eq!(
            deferred
                .ensure_ready("web.myapp.localhost", Duration::ZERO)
                .await,
            Activation::Unknown
        );
        // 落ちないことも確かめる。
        deferred.touch("web.myapp.localhost");
    }

    #[tokio::test]
    async fn forwards_once_set() {
        let deferred = DeferredActivator::new();
        deferred.set(Arc::new(Counting(AtomicUsize::new(0))));

        let result = deferred
            .ensure_ready("web.myapp.localhost", Duration::ZERO)
            .await;

        assert!(matches!(result, Activation::Ready(_)));
    }

    #[tokio::test]
    async fn setting_twice_keeps_the_first() {
        let deferred = DeferredActivator::new();
        deferred.set(Arc::new(Counting(AtomicUsize::new(0))));
        deferred.set(Arc::new(minato_proxy::NoopActivator));

        // 2 回目が効いていれば Unknown になる。
        let result = deferred
            .ensure_ready("web.myapp.localhost", Duration::ZERO)
            .await;
        assert!(matches!(result, Activation::Ready(_)));
    }
}
