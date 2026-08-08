//! The single point of contact between the daemon and its clients.
//!
//! **No human-facing formatting belongs here.** Presentation is the CLI's
//! and the GUI's job. Likewise, no client crate may depend on
//! `minato-runtime` or any other implementation (`docs/DESIGN.md` §3, §13).

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
pub use event::{Event, LogLevel, OutputStream, StepStatus};
pub use protocol::{ClientMessage, Outcome, PROTOCOL_VERSION, RequestId, ServerMessage};
pub use request::{Request, Target};
pub use response::{
    EnvInfo, Pong, PurgeFailure, PurgeProject, PurgeReport, PurgeWorkspace, Response, ServiceInfo,
    TunnelInfo, TunnelLeftover, TunnelState, WorkspaceInfo,
};

/// Re-exported for convenience, so clients need not pull in `minato-core`.
pub use minato_core::{EnvScope, ServiceScope, ServiceState};
