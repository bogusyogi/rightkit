//! Shared agent event model and vendor CLI protocol clients for Right Suite apps.
//!
//! Protocol pieces are sans-IO (`claude`, `codex`); the `session` feature adds the
//! blocking, thread-based session lifecycle on top: spawn through `rightkit-child-process`,
//! typed event stream, host-answered tool approvals, cancel and whole-tree teardown.
//! Nothing in this crate speaks HTTP, links a vendor SDK or uses MCP.
//!
//! - [`event`]: the vendor-neutral [`AgentEvent`] stream.
//! - [`rate_limit`]: rate-limit state (no data, probe failed, unsupported, known).
//! - [`tasks`]: normalized task list, subagent and lane-capacity state; idle lanes are
//!   `Unknown` until every source has reported.
//! - [`discovery`]: vendor CLI lookup on `PATH` and bounded version and sign-in probes.
//! - `claude` (feature `claude`): Claude Code `stream-json` parser, writer and argv builder.
//! - `codex` (feature `codex`): Codex `app-server` JSON-RPC client over stdio.
//! - `session` (feature `session`): `Session` over either backend.

pub mod discovery;
pub mod event;
pub mod rate_limit;
pub mod tasks;

#[cfg(feature = "claude")]
pub mod claude;
#[cfg(feature = "codex")]
pub mod codex;

#[cfg(feature = "driver_host")]
pub mod driver_host;
#[cfg(feature = "session")]
pub mod session;

pub use event::{
    AgentEvent, ControlOutcome, ControlResponse, McpServerStatus, McpStartup, ParseError,
    PermissionDenial, PlanStep, RateLimitInfo, RateLimitWindow, RateLimitWindows, ResultSummary,
    SessionInit, UnknownPolicy, UsageCounts,
};

#[cfg(feature = "agent_host")]
#[allow(missing_docs, clippy::all, rustdoc::all, unused)]
#[cfg(windows)]
pub mod vendor_mcp_pipe;
