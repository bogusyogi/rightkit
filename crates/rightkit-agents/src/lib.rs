//! Shared agent event model and vendor CLI protocol clients for Right Suite apps.
//!
//! Protocol pieces are sans-IO (`claude`, `codex`); the `session` feature adds the
//! blocking, thread-based session lifecycle on top: spawn through `rightkit-child-process`,
//! typed event stream, host-answered tool approvals, cancel and whole-tree teardown.
//! Nothing in this crate speaks HTTP, links a vendor SDK or uses MCP.
//!
//! - [`event`]: the vendor-neutral [`AgentEvent`] stream.
//! - [`rate_limit`]: rate-limit state (no data, probe failed, unsupported, known).
//! - [`discovery`]: vendor CLI lookup on `PATH` and bounded version and sign-in probes.
//! - `claude` (feature `claude`): Claude Code `stream-json` parser, writer and argv builder.
//! - `codex` (feature `codex`): Codex `app-server` JSON-RPC client over stdio.
//! - `session` (feature `session`): `Session` over either backend.

pub mod discovery;
pub mod event;
pub mod rate_limit;

#[cfg(feature = "claude")]
pub mod claude;
#[cfg(feature = "codex")]
pub mod codex;

#[cfg(feature = "session")]
pub mod session;

pub use event::{
    AgentEvent, ControlOutcome, ControlResponse, McpServerStatus, ParseError, PermissionDenial,
    RateLimitInfo, ResultSummary, SessionInit, UnknownPolicy, UsageCounts,
};
