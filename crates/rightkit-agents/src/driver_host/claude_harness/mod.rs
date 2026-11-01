//! The Claude Code harness: the real Claude Code CLI as a persistent
//! the host app root runtime (canon atom `PL-005`, action `REPAIR/WIRE`).
//!
//! **Product boundary (binding, pre-lock reconciliation 2026-08-29):**
//! Claude Code is the only first-party foreign harness. This module is
//! deliberately Claude-only and must not be generalized to other
//! harnesses; every other foreign CLI arrives through the
//! extension/runtime-adapter boundary instead.
//!
//! ## Donor lineage (port-first execution rule)
//!
//! The primary donor is the host app's own `driver_host` CLI-runtime
//! lineage; this module lifts its mechanisms and re-composes them at
//! the persistent-root-runtime seam instead of redesigning them:
//!
//! - Launch policy — `crate::driver_host::claude` (`ClaudeLaunchOptions::launch_spec`),
//!   the frozen argv snapshots (`cli_runtime::transports::_argv_snapshot`)
//!   and the delegation flags (`--tools ""`, `--allowedTools
//!   mcp__coderight`, `--strict-mcp-config`, `--append-system-prompt`
//!   `RUN_SCOPED_POLICY`). The harness validates the *effective* argv
//!   fail-closed before every spawn (`delegation`).
//! - Process supervision — `cli_runtime::transports::process`
//!   (`StdioProcessIo`, Windows job object, sandboxed spawn) and
//!   `cli_runtime::recovery::LiveProcessGeneration` (monotonic restart
//!   generations). The supervisor reuses both verbatim.
//! - Session binding + recovery invariants — `cli_runtime::recovery`
//!   (`BindingSnapshot`, `check_binding_for_resume`) and
//!   `cli_runtime::ids::ContextEpoch`. The harness persists its own
//!   binding record (`binding`) that projects those fields.
//! - Event normalization — `crate::driver_host::parse_claude_stream_line` (the
//!   legacy Claude stream-JSON parser), exact-once admission via
//!   `crate::driver_host::dedupe`. The runtime consumes the same parser; nothing
//!   here re-parses the vendor wire differently.
//! - Delegation policy — `cli_runtime::policy::resolve_child_runtime`
//!   (`delegate_task`/`delegate_parallel` seam). The harness gate is a
//!   thin authoritative wrapper plus a parallel-children ledger.
//! - Claude Code CLI contract (non-Rust donor,
//!   `repos/cli/claude-code`): the stream-JSON structured-I/O contract
//!   (`src/cli/structuredIO.ts`) — interleaved `user` messages for
//!   steering (`prependUserMessage` shape) and the
//!   `control_request`/`control_response` control protocol
//!   (`{"request":{"subtype":"interrupt"}}`). Translated faithfully;
//!   see `runtime::steer_frame` / `runtime::interrupt_frame`.
//!
//! ## Delegation invariant
//!
//! Claude-root child work must flow through the host app delegation. The
//! seam is written so provider-native child orchestration cannot
//! bypass the host app policy, isolation, budgets, task ownership, event
//! projection, cancellation or persistence:
//!
//! 1. every launch is validated to expose *only* the the host app MCP
//!    gateway as a tool surface (the native Task/agent tool surface is
//!    removed by `--tools ""` + `--allowedTools mcp__coderight`), and
//! 2. any Claude-native child-spawn tool call that still surfaces in
//!    the event stream is detected (`delegation::inspect_tool_use_for_native_child_spawn`),
//!    counted on the persisted binding, and projected as a typed
//!    `DelegationBypassDetected` progress event, and
//! 3. every child spawn is resolved through
//!    `cli_runtime::policy::resolve_child_runtime` via
//!    `delegation::delegate_through_coderight`, which records a
//!    receipt (backend, resolution source, session-policy digest) and
//!    enforces the session's `max_parallel_children` budget
//!    ([`delegation::DelegationLedger`]).

pub mod binding;
pub mod delegation;
pub mod runtime;
pub mod supervisor;
pub mod workspace;

#[cfg(test)]
pub(crate) mod test_support;

pub use binding::{
    transition_is_valid, BindingMetadataStore, BindingStoreError, ClaudeHarnessBindingRecord,
    HarnessBindingStatus, InMemoryBindingMetadataStore, JsonFileBindingMetadataStore,
    HARNESS_ENGINE,
};
pub use delegation::{
    delegate_through_host, enforce_delegation_launch_policy,
    enforce_delegation_launch_policy_for_legacy, inspect_tool_use_for_native_child_spawn,
    policy_digest, session_policy_digest, DelegationLedger, DelegationReceipt,
    LaunchPolicyViolation, NativeChildSpawn, HOST_MCP_TOOL_SCOPE, NATIVE_CHILD_SPAWN_TOOL_NAMES,
};
pub use runtime::{
    interrupt_frame, project_progress_kinds, steer_frame, ClaudeRootRuntime, ControlKind,
    ControlRejection, ControlState, HarnessProgressEvent, HarnessProgressKind,
};
pub use supervisor::{ClaudeHarnessSupervisor, HarnessSpawner, SupervisedProcessInfo};
pub use workspace::{
    harness_denied_env_vars, managed_claude_home, TrustBoundary, WorkspaceTrustVerdict,
};

use crate::driver_host::cli_runtime::adapter::CliRuntimeError;
use crate::driver_host::contracts::DriverHostError;

/// Wall-clock timestamp helper. The crate has no chrono/time
/// dependency; persisted binding metadata therefore carries unix
/// epoch milliseconds (matching `expires_at_unix_ms` on
/// `crate::driver_host::contracts::CapabilityRecord`).
pub fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// Typed errors raised by the Claude harness. Every variant is
/// actionable at the daemon seam: trust and policy failures are
/// fail-closed preconditions, transport failures wrap the shared
/// `CliRuntimeError`, and store failures wrap
/// [`BindingStoreError`].
#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("claude harness trust boundary: {0}")]
    Trust(String),
    #[error("claude harness delegation policy: {0}")]
    Delegation(String),
    #[error("claude harness: steering rejected: {0}")]
    SteerRejected(String),
    #[error("claude harness: vendor protocol parse failed: {0}")]
    Parse(String),
    #[error("claude harness: vendor transport {code}: {message}")]
    Transport { code: String, message: String },
    #[error("claude harness: binding metadata store: {0}")]
    Store(#[from] BindingStoreError),
    #[error("claude harness: runtime is not running")]
    NotRunning,
    #[error("claude harness: runtime is already running")]
    AlreadyRunning,
    #[error("claude harness: illegal binding status transition {from:?} -> {to:?}")]
    InvalidTransition {
        from: HarnessBindingStatus,
        to: HarnessBindingStatus,
    },
    #[error("claude harness: {0}")]
    InvalidState(String),
}

impl From<CliRuntimeError> for HarnessError {
    fn from(error: CliRuntimeError) -> Self {
        HarnessError::Transport {
            code: error.code,
            message: error.message,
        }
    }
}

impl From<DriverHostError> for HarnessError {
    fn from(error: DriverHostError) -> Self {
        match error {
            DriverHostError::InvalidMessage { message, .. } => HarnessError::Parse(message),
            DriverHostError::UnknownRequiredEvent { method, .. } => {
                HarnessError::Parse(format!("unknown required vendor event: {method}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harness_errors_render_actionable_messages() {
        let trust = HarnessError::Trust("workspace root must be absolute".to_string());
        assert!(trust.to_string().contains("trust boundary"));
        let transport = HarnessError::Transport {
            code: "cli_process_spawn".to_string(),
            message: "failed to start claude".to_string(),
        };
        assert!(transport.to_string().contains("cli_process_spawn"));
        let parse = HarnessError::from(DriverHostError::UnknownRequiredEvent {
            driver: crate::driver_host::contracts::DriverKind::Claude,
            method: "system:future_subtype".to_string(),
        });
        assert!(parse.to_string().contains("system:future_subtype"));
    }

    #[test]
    fn now_unix_ms_is_monotonic_within_reason() {
        let first = now_unix_ms();
        let second = now_unix_ms();
        assert!(second >= first, "unix-ms clock must not run backwards");
    }
}
