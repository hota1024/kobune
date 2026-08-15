//! The tunnel services Kobune can drive.
//!
//! Two, and deliberately the two that are shaped differently: one is
//! handed a zone of yours and derives every name from it, the other has a
//! domain of its own and gives out one name per service. What they have
//! in common is [`crate::TunnelProvider`], and what they do not is
//! [`crate::Needs`].

pub mod cloudflare;
pub mod quick;

pub use cloudflare::CloudflareProvider;
pub use quick::QuickProvider;
