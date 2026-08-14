//! The single point of contact between the daemon and its clients.
//!
//! **No human-facing formatting belongs here.** Presentation is the CLI's
//! and the GUI's job. Likewise, no client crate may depend on
//! `kobune-runtime` or any other implementation (`docs/DESIGN.md` §3, §13).

pub mod codec;
pub mod diagnostics;
pub mod error;
pub mod event;
pub mod protocol;
pub mod request;
pub mod response;

pub use codec::{CodecError, MessageStream, write_message};
pub use diagnostics::{Check, CheckStatus, Diagnostics};
pub use error::{ApiError, ErrorCode};
pub use event::{Event, LogLevel, OutputStream, StepStatus, decode_bytes, encode_bytes};
pub use protocol::{ClientMessage, Outcome, PROTOCOL_VERSION, RequestId, ServerMessage, Typed};
pub use request::{Request, Target, Window};
pub use response::{
    EnvInfo, Pong, PurgeFailure, PurgeProject, PurgeReport, PurgeStorageFailure, PurgeVolume,
    PurgeWorkspace, Response, ServiceInfo, TunnelInfo, TunnelLeftover, TunnelState, Unsettled,
    UnsettledReason, WorkspaceInfo,
};

/// Re-exported for convenience, so clients need not pull in `kobune-core`.
pub use kobune_core::{EnvScope, ServiceScope, ServiceState};
