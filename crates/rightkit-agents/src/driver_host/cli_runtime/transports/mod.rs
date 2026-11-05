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
//! preserved; the `_argv_snapshot` module freezes the pre-refactor
//! argv lists so the byte-equivalence tests fail if the transports
//! ever drift.

pub mod acp;
pub mod claude_stream_json;
pub mod codex_app_server;
pub mod process;
pub mod resumable_ndjson;
