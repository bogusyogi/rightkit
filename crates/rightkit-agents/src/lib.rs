//! Shared agent event model and vendor CLI protocol clients for Right Suite apps.
//!
//! Lane 1 of the `rightkit-agents` design: sans-IO protocol pieces only. There is no
//! session lifecycle, no process spawning and no approvals here yet; those arrive in
//! later lanes. Nothing in this crate speaks HTTP, links a vendor SDK or uses MCP.
//!
//! - [`event`]: the vendor-neutral [`AgentEvent`] stream.
//! - `claude` (feature `claude`): Claude Code `stream-json` parser, writer and argv builder.
//! - `codex` (feature `codex`): Codex `app-server` JSON-RPC client over stdio.

pub mod event;

#[cfg(feature = "claude")]
pub mod claude;
#[cfg(feature = "codex")]
pub mod codex;

pub use event::{AgentEvent, ParseError, PermissionDenial, UsageCounts};
