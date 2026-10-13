# Changelog

## 0.1.0

- First release. Lane 1 of the rightkit-agents design: protocol clients and fixtures, no session lifecycle yet.
- `AgentEvent`: one vendor-neutral, `#[non_exhaustive]` event model (init, text, thinking, tool use and result, subagent start and finish, approvals, usage, compaction, result with permission denials, stderr, unknown). Unknown vendor messages surface as `Unknown`; they never fail parsing.
- Feature `claude`: Claude Code `stream-json` `StreamParser`, stdin frame writers, and `build_args` with `ConfigMode::Full` (user config loads) or `ConfigMode::Bare` (`--bare`). Lifted from CodeRight's driver_host and adapted.
- Feature `codex`: Codex `app-server` JSON-RPC `CodexClient` over any reader and writer, with the schema pinned to codex-cli 0.161.0. Lifted from CodeRight's driver_host and adapted.
- Recorded fixtures (subagents, permission denials, budget hit, approvals, the 0.161.0 schema snapshot) and a test-only fake CLI that replays them over stdio. Nothing test-only ships.
- Scope invariant test: no HTTP client, async runtime, vendor SDK, MCP or Node dependency, and no vendor API host in the source.
- Nothing in this release was compiled or run when written; CI is the first compile.
