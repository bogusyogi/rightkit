# Changelog

## 0.2.0

- Feature `session` (new, pulls in `claude`, `codex` and `rightkit-child-process`): `session::Session`, a blocking thread-based session over either vendor CLI. `Session::spawn(SessionConfig)` starts the CLI through `OwnedCommand` (it owns its process tree), `events()` is a single-consumer `mpsc::Receiver<SessionEvent>` of vendor-neutral `AgentEvent`s plus `Error` and `Closed`, `send_user_message`, `approve` / `deny` (`ApprovalDecision::{Allow, AllowForSession, Deny { message, interrupt }}`), `cancel` (interrupt the turn, release pending approvals), `pending_approvals`, `session_id`, `pid`, `exit_status` and `shutdown(grace)` (close stdin, polite stop, forced tree kill, reap; dropping the `Session` also kills the tree). Attach to an existing conversation with `ClaudeOptions::resume_session_id` or `CodexOptions::resume_thread_id`.
- `claude`: `ClaudeOptions::host_approvals` (adds `--permission-prompt-tool stdio`), `permission_allow_frame`, `permission_deny_frame`. The one spelling of that flag is the literal `stdio` control channel, never an MCP tool; `tests/scope.rs` now pins that instead of banning the flag.
- `codex`: `CodexOptions`, `build_args`, `thread_params`, `approval_response_frame`, `error_response_frame`; `CodexClient::respond_approval` reuses the frame builder. Server requests that are not approvals are refused with JSON-RPC `-32601` so the server never waits on them.
- Tests: `tests/session.rs` drives both backends against a scripted fake CLI (the test binary re-executing itself); no real `claude` or `codex` is run.
- Unverified (nothing was compiled or run when written; CI is the first compile, and neither CLI was run): the `claude -p --input-format stream-json --permission-prompt-tool stdio` handshake and the `can_use_tool` request / `control_response` shapes follow the Agent SDK docs and the fixtures, not a recorded live session; Claude has no wire id for allow-for-session, so it is treated as allow; `control_cancel_request` is not handled (a pending approval the CLI withdrew stays listed until answered or `cancel`); Codex `app-server` method names, the `turn/started` turn id location and decision strings follow the 0.161.0 schema snapshot and the public app-server page, not a live run.

## 0.1.0

- First release. Lane 1 of the rightkit-agents design: protocol clients and fixtures, no session lifecycle yet.
- `AgentEvent`: one vendor-neutral, `#[non_exhaustive]` event model (init, text, thinking, tool use and result, subagent start and finish, approvals, usage, compaction, result with permission denials, stderr, unknown). Unknown vendor messages surface as `Unknown`; they never fail parsing.
- Feature `claude`: Claude Code `stream-json` `StreamParser`, stdin frame writers, and `build_args` with `ConfigMode::Full` (user config loads) or `ConfigMode::Bare` (`--bare`). Lifted from CodeRight's driver_host and adapted.
- Feature `codex`: Codex `app-server` JSON-RPC `CodexClient` over any reader and writer, with the schema pinned to codex-cli 0.161.0. Lifted from CodeRight's driver_host and adapted.
- Recorded fixtures (subagents, permission denials, budget hit, approvals, the 0.161.0 schema snapshot) and a test-only fake CLI (the test binary re-executes itself and replays a fixture over stdio; the parent skips the libtest banner up to a sentinel line, parsers stay strict). Nothing test-only ships.
- Scope invariant test: no HTTP client, async runtime, vendor SDK, MCP or Node dependency, and no vendor API host in the source.
- Nothing in this release was compiled or run when written; CI is the first compile.
