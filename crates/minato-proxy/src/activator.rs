//! The hook for waking a stopped service.
//!
//! The proxy cannot depend on `minato-runtime` (`docs/DESIGN.md` §13), yet
//! "start it when a request arrives" needs the runtime. This trait is the
//! boundary; the daemon supplies the implementation. All the proxy does is
//! ask for a host to be made ready.

use std::net::SocketAddr;
use std::time::Duration;

use async_trait::async_trait;

/// The outcome of a wake request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activation {
    /// Ready. Forward here.
    Ready(SocketAddr),

    /// Starting, but not yet ready.
    ///
    /// Browsers get a waiting page; everyone else waits.
    Starting,

    /// No such host.
    Unknown,

    /// Tried to start, and failed.
    Failed(String),
}

#[async_trait]
pub trait Activator: Send + Sync + 'static {
    /// Makes the service behind a host ready to serve.
    ///
    /// Already running: returns the target and does nothing. Stopped:
    /// starts it.
    ///
    /// `wait` bounds how long to wait for readiness — short for browsers,
    /// which then get a waiting page, longer for everyone else. Returns
    /// [`Activation::Starting`] on timeout; the start continues regardless.
    async fn ensure_ready(&self, host: &str, wait: Duration) -> Activation;

    /// Records that the host was accessed.
    ///
    /// This is what idle detection measures. Called on every request, so
    /// it **must be fast**: hold no lock for long and do no I/O.
    fn touch(&self, host: &str);
}

/// An implementation that wakes nothing. For tests, and for setups
/// without scale-to-zero.
pub struct NoopActivator;

#[async_trait]
impl Activator for NoopActivator {
    async fn ensure_ready(&self, _host: &str, _wait: Duration) -> Activation {
        Activation::Unknown
    }

    fn touch(&self, _host: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn noop_knows_nothing() {
        let activator = NoopActivator;

        assert_eq!(
            activator
                .ensure_ready("web.myapp.localhost", Duration::from_millis(1))
                .await,
            Activation::Unknown
        );
        activator.touch("web.myapp.localhost");
    }
}
