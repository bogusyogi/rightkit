//! Byte-equivalent argv snapshots of the current Claude and Codex launches.
//!
//! PR 6 of the architecture plan (`docs/architecture/CLI-RUNTIME-SCOPE.md`).
//!
//! This module freezes the pre-refactor argv lists emitted by
//! [`crate::driver_host::ClaudeLaunchOptions::launch_spec`] and
//! [`crate::driver_host::CodexLaunchOptions::launch_spec`] so the new transport
//! layer can be tested for byte-equivalence. The transports in
//! [`super::claude_stream_json`] and [`super::codex_app_server`]
//! both delegate their argv construction to those legacy entry
//! points — if the snapshots ever drift from the legacy argv, the
//! matching transport test fails and the refactor is forced to
//! explain why.
//!
//! The snapshots cover the canonical launch shape used by the
//! installed smoke tests (one session, no model override, baseline
//! `--strict-mcp-config`/`--strict-config` flags). The model and
//! resume-snapshot variants live alongside the legacy tests and are
//! not duplicated here; the byte-equivalence tests in the transport
//! modules cover both the baseline and the variant shapes by
//! calling the transport with multiple inputs and asserting equality
//! against the live legacy output.

/// The argv the Claude stream-JSON transport must produce for a
/// first-turn launch with a fresh session id and the canonical
/// host app MCP config. Captured verbatim from the legacy
/// `claude.rs` test that asserts the current behavior.
///
/// Order follows RightKit's published Claude argv builder (`build_args_with`), which the host app
/// now adopts; the host app MCP flags are appended after it. Values are unchanged.
pub const CLAUDE_BASELINE_ARGV: &[&str] = &[
    "--print",
    "--input-format",
    "stream-json",
    "--output-format",
    "stream-json",
    "--verbose",
    "--include-partial-messages",
    "--session-id",
    "550e8400-e29b-41d4-a716-446655440000",
    "--tools",
    "",
    "--no-chrome",
    "--allowedTools",
    "mcp__rightkit",
    "--append-system-prompt",
    crate::driver_host::RUN_SCOPED_POLICY,
    "--strict-mcp-config",
    "--mcp-config",
    r#"{"mcpServers":{"host":{"args":[],"command":"/usr/local/bin/host-mcp"}}}"#,
];

/// The argv the Codex app-server transport must produce for a
/// baseline launch with `--strict-config` and no model override.
/// Captured verbatim from the legacy `codex.rs` test that asserts
/// the current behavior.
pub const CODEX_BASELINE_ARGV: &[&str] = &["app-server", "--stdio", "--strict-config"];

/// The argv the Codex app-server transport must produce when a
/// caller-selected model id is supplied. Captured verbatim from the
/// legacy `codex.rs` test.
pub const CODEX_WITH_MODEL_ARGV: &[&str] = &[
    "-c",
    "model=\"o3\"",
    "app-server",
    "--stdio",
    "--strict-config",
];

/// The argv the Claude stream-JSON transport must produce when
/// resuming an existing session instead of starting a new one.
/// Captured verbatim from the legacy `claude.rs` test.
///
/// Argv order as produced by the RightKit-backed builder (see `CLAUDE_BASELINE_ARGV`).
pub const CLAUDE_RESUME_ARGV: &[&str] = &[
    "--print",
    "--input-format",
    "stream-json",
    "--output-format",
    "stream-json",
    "--verbose",
    "--include-partial-messages",
    "--resume",
    "existing-session",
    "--tools",
    "",
    "--no-chrome",
    "--allowedTools",
    "mcp__rightkit",
    "--append-system-prompt",
    crate::driver_host::RUN_SCOPED_POLICY,
    "--strict-mcp-config",
    "--mcp-config",
    r#"{"mcpServers":{"host":{"args":[],"command":"/usr/local/bin/host-mcp"}}}"#,
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The snapshot is the source of truth for byte-equivalence.
    /// A contributor who edits `CLAUDE_BASELINE_ARGV` here is
    /// expected to update the legacy `claude.rs` test in the same
    /// change; the transport test in
    /// `super::claude_stream_json` will fail otherwise.
    #[test]
    fn claude_snapshot_includes_canonical_session_id_flag_pair() {
        // The flag/value pair is the durability anchor: removing it
        // changes the vendor session identity for every Claude
        // launch.
        assert!(
            CLAUDE_BASELINE_ARGV
                .windows(2)
                .any(|pair| pair == ["--session-id", "550e8400-e29b-41d4-a716-446655440000"]),
            "baseline snapshot must carry the canonical --session-id flag pair"
        );
    }

    /// The Codex baseline argv must end with the three-flag shape
    /// the daemon's installed smoke test asserts on stdout. Drift
    /// here is a hard launch breakage.
    #[test]
    fn codex_snapshot_is_three_flag_shape() {
        assert_eq!(
            CODEX_BASELINE_ARGV,
            ["app-server", "--stdio", "--strict-config"],
            "codex baseline argv must match the installed smoke test"
        );
    }

    /// Model override must remain a `-c model="<id>"` pair before
    /// the `app-server` subcommand. Anything else changes the
    /// effective config and is a security-relevant diff.
    #[test]
    fn codex_model_override_precedes_subcommand() {
        let idx = CODEX_WITH_MODEL_ARGV
            .iter()
            .position(|arg| *arg == "app-server")
            .expect("app-server subcommand must be present");
        assert!(
            CODEX_WITH_MODEL_ARGV[..idx] == ["-c", "model=\"o3\""],
            "model override must precede app-server subcommand"
        );
    }

    /// Resume replaces the `--session-id` flag with `--resume`. The
    /// snapshot freezes that invariant.
    #[test]
    fn claude_resume_snapshot_has_resume_pair_and_no_session_id() {
        assert!(
            CLAUDE_RESUME_ARGV
                .windows(2)
                .any(|pair| pair == ["--resume", "existing-session"]),
            "resume snapshot must carry --resume flag pair"
        );
        assert!(
            !CLAUDE_RESUME_ARGV.contains(&"--session-id"),
            "resume snapshot must not also carry --session-id"
        );
    }
}
