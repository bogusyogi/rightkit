//! Transport implementations, keyed by transport kind rather than
//! provider identity. Multiple providers may share a transport; ACP
//! in particular is shared across Grok, Cursor, and Cline.
//!
//! PR 1 of the architecture plan declared the transport marker
//! types; PR 6 implements the Claude stream-JSON and Codex
//! app-server transports behind the [`crate::driver_host::cli_runtime::adapter::CliTransport`]
//! trait and moves argv construction and event parsing behind the
//! transport interface. The new transports delegate argv construction
//! to the legacy [`crate::driver_host::ClaudeLaunchOptions::launch_spec`] and
//! [`crate::driver_host::CodexLaunchOptions::launch_spec`] so byte-equivalence is
//! preserved; the [`_argv_snapshot`] module freezes the pre-refactor
//! argv lists so the byte-equivalence tests fail if the transports
//! ever drift.

#[cfg(test)]
pub(crate) mod _argv_snapshot;
pub mod acp;
pub mod claude_stream_json;
pub mod codex_app_server;
pub mod process;
pub mod resumable_ndjson;

// Re-export the byte-equivalence snapshots so tests in
// `claude_stream_json` and `codex_app_server` can import them as
// `super::argv_snapshot::CLAUDE_BASELINE_ARGV` etc.
#[cfg(test)]
pub(crate) use _argv_snapshot as argv_snapshot;
