#![allow(
    dead_code,
    unused_variables,
    unused_imports,
    unused_mut,
    clippy::type_complexity,
    clippy::match_like_matches_macro,
    clippy::manual_contains,
    clippy::match_result_ok,
    clippy::needless_borrow,
    clippy::ptr_arg
)]

//! Vendor-neutral contracts for hosting subscription-authenticated coding drivers.
//!
//! The crate deliberately contains no process supervisor or the host app daemon state. It owns the
//! stable wire boundary: launch specifications, persisted bindings/cursors, vendor event parsing,
//! and exact-once admission. The daemon supplies persistence and execution capabilities.

pub mod agy_harness;
mod binary_lookup;
mod claude;
pub mod claude_harness;
mod claude_permission;
pub mod cli_runtime;
mod codex;
mod contracts;
mod dedupe;
mod rate_limit_state;
pub mod vendor_cli;

pub use claude::{
    apply_claude_account_env, parse_claude_assistant_tool_uses, parse_claude_stream_events,
    parse_claude_stream_line, stamp_account, ClaudeAccountIdentity, ClaudeAccountLaunch,
    ClaudeLaunchExtras, ClaudeLaunchOptions, ClaudeMcpServerStatus, ClaudeModelUsage,
    ClaudeRunFlags, ClaudeSessionInit, ClaudeToolUse,
};
pub use claude_permission::{
    claude_permission_response_frame, enable_stdio_permission_prompt_for_spec,
    parse_claude_can_use_tool, ClaudePermissionDecision, ClaudePermissionRequest,
};
pub use codex::{
    apply_codex_account_env, parse_codex_jsonrpc_line, parse_codex_usage_limit_event,
    CodexLaunchOptions, CodexMessage,
};
pub use contracts::*;
pub use dedupe::{DedupAdmission, EventDeduplicator, InMemoryEventDeduplicator};
pub use rate_limit_state::{RateLimitSnapshot, RateLimitState};

/// The single policy injected into each vendor run without changing global vendor configuration.
pub const RUN_SCOPED_POLICY: &str = "Agent is the sole authority for execution, approvals, memory writes, and delegation in this run. Use the rightkit-agent MCP server for every shell command, file mutation, memory write, and delegation. Native tools may only perform read-only inspection. Never request or perform a native side effect; ask Agent through its capability gateway.";

/// Environment variable names stripped from every vendor subprocess before launch.
///
/// A vendor CLI (Codex, Claude) is a different trust domain from the the host app daemon: it must
/// never be able to read the provider keys that pay for the host app's own model calls, nor the
/// daemon's bootstrap auth token, which is the only gate on the daemon's mutating HTTP API. Both
/// `CodexLaunchOptions::launch_spec` and `ClaudeLaunchOptions::launch_spec` populate
/// `LaunchSpec::env_remove` from this single constant so the two drivers cannot drift apart. The
/// spawn site applies `env_remove` before `env` (see `driver_host_runtime`), so a value the host app
/// deliberately re-grants afterward — e.g. a scoped `RIGHTKIT_DAEMON_TOKEN` issued for this one
/// run's MCP callback — still reaches the child; only ambient inheritance is denied.
pub const DENIED_VENDOR_SUBPROCESS_ENV_VARS: &[&str] = &[
    "OPENROUTER_API_KEY",
    "MINIMAX_API_KEY",
    "TINYFISH_API_KEY",
    "RIGHTKIT_DAEMON_TOKEN",
    "RIGHTKIT_GATEWAY_TOKEN",
    "RIGHTKIT_DRIVER_INSTANCE_SECRET",
];

/// Validate a caller-supplied model id before it is interpolated into a vendor CLI argv.
///
/// This is a defense against argv/shell injection through a hostile model string, not a live
/// catalogue check — only conservative identifier characters are allowed (ASCII alphanumerics
/// plus `. - _ : / @`), and leading/trailing whitespace or an empty string are rejected.
pub(crate) fn validate_model_id(id: &str) -> Result<&str, String> {
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return Err("model id must not be empty".to_string());
    }
    if trimmed.len() != id.len() {
        return Err("model id must not have leading or trailing whitespace".to_string());
    }
    let is_plausible = trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '/' | '@'));
    if !is_plausible {
        return Err(format!(
            "model id contains disallowed characters: {trimmed}"
        ));
    }
    Ok(trimmed)
}
