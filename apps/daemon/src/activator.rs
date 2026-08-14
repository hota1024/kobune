//! Wiring the proxy's wake requests through to the supervisor.
//!
//! The gateway needs an activator to start, and the supervisor needs the
//! gateway to issue URLs. [`DeferredActivator`] breaks that cycle by
//! taking the real implementation later.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use kobune_proxy::{Activation, Activator};

use crate::supervisor::Supervisor;

/// Presents the supervisor as an activator.
///
/// The orphan rule rules out `impl Activator for Arc<Supervisor>`, so a
/// newtype goes in between.
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

/// An activator whose implementation arrives later.
///
/// Called before it does, every host is an unknown one. That window is
/// vanishingly short, right after the daemon starts.
#[derive(Clone, Default)]
pub struct DeferredActivator {
    inner: Arc<OnceLock<Arc<dyn Activator>>>,
}

impl DeferredActivator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Supplies the implementation. Later calls are ignored.
    pub fn set(&self, activator: Arc<dyn Activator>) {
        if self.inner.set(activator).is_err() {
            tracing::warn!("the activator is already set");
        }
    }
}

#[async_trait]
impl Activator for DeferredActivator {
    async fn ensure_ready(&self, host: &str, wait: Duration) -> Activation {
        match self.inner.get() {
            Some(activator) => activator.ensure_ready(host, wait).await,
            None => {
                tracing::debug!("the activator is not set yet: {host}");
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
        // Also confirms it does not panic.
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
        deferred.set(Arc::new(kobune_proxy::NoopActivator));

        // Had the second call taken, this would be Unknown.
        let result = deferred
            .ensure_ready("web.myapp.localhost", Duration::ZERO)
            .await;
        assert!(matches!(result, Activation::Ready(_)));
    }
}
