//! Making an environment reachable from outside, over one tunnel service
//! or another.
//!
//! The daemon asks for a tunnel and is told what to pass on. Which service
//! carries it, what that service had to be told, and what it leaves in
//! somebody's account are all [`TunnelProvider`]'s — the same split
//! `kobune-runtime` makes for Docker and Apple Container, and for the same
//! reason: the layer above should not be able to tell them apart.
//!
//! What every provider shares is that it says where a service answers.
//! Not what the answer looks like: a zone of the user's derives every
//! name from the domain in the request, and a service with a domain of
//! its own hands out one name at a time and covers nothing it was not
//! asked about. [`Hostnames`] is where those two meet, and the daemon
//! reads it rather than deriving anything itself.

pub mod provider;
pub mod providers;

#[cfg(test)]
mod testing;

use std::path::PathBuf;

pub use provider::{
    Access, Hostnames, Leftover, Missing, Needs, Readiness, RunningTunnel, StartOutcome, Started,
    TunnelProvider, TunnelRequest, TunnelTarget,
};
pub use providers::{CloudflareProvider, QuickProvider};

/// The default named tunnel.
///
/// One per machine. Reusing the name means `tunnel enable` is idempotent
/// across projects.
pub const DEFAULT_TUNNEL_NAME: &str = "kobune";

/// The providers that can be named.
///
/// The list lives in `kobune-core` because the CLI validates `--provider`
/// against it and may not reach this crate. What keeps the two honest is
/// [`create`], which every name here is put through in this crate's own
/// tests.
pub const AVAILABLE_PROVIDERS: &[&str] = kobune_core::TUNNEL_PROVIDERS;

/// Builds a provider from its identifier.
///
/// Nothing is connected to and nothing is looked up, so success here does
/// not mean the provider is usable — that is [`TunnelProvider::readiness`],
/// which the daemon asks separately and reports rather than fails on.
///
/// An unknown identifier is an error rather than a fallback. It reaches
/// here from `TunnelRecord.provider`, which is a string precisely so an
/// unrecognised value loads; turning it into Cloudflare at this point
/// would start the wrong tunnel and report success.
pub fn create(id: &str) -> Result<Box<dyn TunnelProvider>> {
    match id {
        providers::cloudflare::ID => Ok(Box::new(CloudflareProvider::new())),
        providers::quick::ID => Ok(Box::new(QuickProvider::new())),
        other => Err(TunnelError::Unsupported(format!(
            "no such tunnel provider `{other}`. Use {}",
            AVAILABLE_PROVIDERS.join(" or ")
        ))),
    }
}

/// The hint to offer when a provider fails, in that provider's own words.
///
/// A failure carries no provider of its own — the same `NotInstalled`
/// comes from any of them — so the caller that knows which one it asked
/// supplies it here.
pub fn error_hint(id: &str, err: &TunnelError) -> Option<String> {
    match id {
        providers::cloudflare::ID => providers::cloudflare::error_hint(err),
        // The quick tunnel drives the same binary, so the same advice
        // applies — minus the login it never asks anyone for. Told to run
        // `cloudflared tunnel login`, somebody would come back from it to
        // the same failure, having authorised a tunnel that was never
        // waiting on one.
        providers::quick::ID => match err {
            TunnelError::NotLoggedIn => None,
            other => providers::cloudflare::error_hint(other),
        },
        _ => None,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TunnelError {
    #[error("no `{0}` command found")]
    NotInstalled(String),

    #[error("cloudflared is not logged in")]
    NotLoggedIn,

    #[error("{0}")]
    Unsupported(String),

    #[error("{operation} failed: {message}")]
    Failed { operation: String, message: String },

    #[error("cannot write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl TunnelError {
    pub fn failed(operation: impl Into<String>, message: impl std::fmt::Display) -> Self {
        Self::Failed {
            operation: operation.into(),
            message: message.to_string(),
        }
    }
}

pub type Result<T, E = TunnelError> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_provider_is_the_one_that_was_always_there() {
        // `TunnelRecord.provider` defaults to this for every state file
        // written before there was a choice, so the two have to agree or
        // an existing tunnel stops being buildable.
        let provider = create(kobune_core::DEFAULT_TUNNEL_PROVIDER).expect("builds");

        assert_eq!(provider.id(), kobune_core::DEFAULT_TUNNEL_PROVIDER);
    }

    #[test]
    fn every_listed_provider_can_be_built() {
        for id in AVAILABLE_PROVIDERS {
            let provider = create(id).unwrap_or_else(|err| panic!("{id} does not build: {err}"));
            assert_eq!(&provider.id(), id);
        }
    }

    #[test]
    fn the_provider_that_never_logs_in_is_not_told_to_log_in() {
        // It drives the same binary as the named tunnel, so most of that
        // provider's advice is right for it too. A login is the
        // exception: a quick tunnel never waits on one, and somebody sent
        // to do it would come back to the same failure having authorised
        // nothing that was asking.
        assert!(error_hint(providers::quick::ID, &TunnelError::NotLoggedIn).is_none());

        let shared = error_hint(
            providers::quick::ID,
            &TunnelError::NotInstalled("cloudflared".into()),
        );
        assert!(
            shared.is_some_and(|hint| hint.contains("cloudflared")),
            "what the two do share is still said"
        );
    }

    #[test]
    fn an_unknown_provider_is_refused_rather_than_guessed() {
        // It arrives from a state file, which keeps an unrecognised value
        // rather than failing to load. Quietly running Cloudflare instead
        // would put an environment on a zone nobody asked for and report
        // success.
        let Err(err) = create("from-the-future") else {
            panic!("an unknown provider must not build");
        };

        assert!(err.to_string().contains("from-the-future"), "got: {err}");
        assert!(
            err.to_string()
                .contains(kobune_core::DEFAULT_TUNNEL_PROVIDER),
            "it says what there is instead: {err}"
        );
    }
}
