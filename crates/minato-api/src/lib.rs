//! daemon とクライアント（CLI / GUI）の唯一の接点。
//!
//! この crate に**人間向けの整形を持ち込まない**。表示は CLI と GUI が
//! それぞれ担当する。同様に、クライアント側の crate が `minato-runtime` などの
//! 実装に依存してはならない（`docs/DESIGN.md` §3, §13）。

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
pub use response::{Pong, Response, ServiceInfo, WorkspaceInfo};

/// 便宜上の再エクスポート。クライアントが `minato-core` を直接引かずに済む。
pub use minato_core::{ServiceScope, ServiceState};
