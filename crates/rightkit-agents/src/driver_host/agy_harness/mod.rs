//! The Antigravity harness: the `agy` CLI as a vendor runtime for the host app
//! turns, mirroring the role `claude_harness` plays for Claude Code.
//!
//! ## Donor lineage
//!
//! Same port-first rule as the Claude harness: the launch and event vocabulary
//! is lifted from `crate::driver_host::claude` (`ClaudeLaunchOptions::launch_spec`,
//! `parse_claude_stream_line`) and the shared `LaunchSpec`/`NormalizedEvent`
//! contracts, re-composed against the `agy` CLI's own flag surface rather
//! than generalized into a shared abstraction.
//!
//! ## CLI contract (confirmed against `agy --help`, v1.2.16)
//!
//! - `-p/--print` runs a non-interactive turn; `--output-format stream-json`
//!   emits one NDJSON event per line on stdout (`init`, `step_update`,
//!   `result` — see `events`).
//! - `--model <id>` selects the model; `agy models` lists the live catalogue.
//! - `--conversation <id>` resumes a previous conversation (session
//!   continuity); `--continue` resumes the most recent one.
//! - `--add-dir <path>` grants the agent access to directories beyond the cwd.
//! - `--dangerously-skip-permissions` auto-approves tool permission requests
//!   (the host app's `Bypass`/yolo permission mode); every other mode keeps the
//!   vendor's default `request-review` policy.
//! - `agy mcp add|list|remove` manages MCP servers in the USER-level config.
//!   Unlike Claude Code there is no per-invocation `--mcp-config` flag, so the
//!   the host app gateway is registered once, idempotently, as a uniquely-named
//!   `host app` stdio server (`mcp`). The registration is therefore shared
//!   user state rather than a run-scoped launch flag — it cannot carry
//!   per-turn arguments, so it is registered without `--allow-mutate` and the
//!   vendor's own permission policy governs mutations (see `mcp`).
//!
//! ## Deliberate narrowing vs `claude_harness`
//!
//! `claude_harness` additionally carries a durable binding store, a managed
//! vendor home and a process supervisor because the daemon owns long-lived
//! Claude root runtimes. `agy` currently runs per-turn (spawn, stream, exit,
//! resume via `--conversation`), so this module keeps only the pieces a
//! per-turn driver needs: launch (`launch`), event normalization (`events`),
//! and user-level MCP registration (`mcp`).

pub mod events;
pub mod launch;
pub mod mcp;

pub use events::parse_agy_stream_line;
pub use launch::{user_prompt_frame, AgyLaunchOptions};
pub use mcp::{ensure_host_mcp_registered, HOST_MCP_SERVER_NAME};
