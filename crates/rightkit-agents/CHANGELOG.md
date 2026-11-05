# Changelog

## 0.2.4

Wave 2 batch w2-01 (CodeRight port): issues #567, #571, #695, #706, #707, #1117, #1118, #1292.
Source-level only: nothing here was compiled or run; the gate is the first compile.

- Build fixes: `event.rs` imports `BTreeMap` and `rate_limit.rs` imports `RateLimitInfo`; `claude.rs` imports `Receiver` for `answer_can_use_tool`. These were missing at 0.2.3 (#571).
- #567: `claude::ClaudeExtras` gains `mcp_config` (`--mcp-config`, refused when empty or starting with `-`), `strict_mcp_config` (`--strict-mcp-config`) and `account: Option<ClaudeAccount>`. `ClaudeAccount::{OAuthToken, ApiKey}` sets one credential variable and removes the other (`claude::launch_env`); `SessionConfig` applies it to Claude sessions. The credential is redacted in `Debug`. Empty credentials are refused by `build_args_with`.
- #571: `AgentEvent::ApprovalRequested` gains `tool_use_id`, `agent_id` and `blocked_path` (Claude `can_use_tool` fields, unverified wire placement; Codex sets `tool_use_id` from `itemId`). Breaking for exhaustive struct patterns; use `..`. `answer_can_use_tool` keeps the `Receiver` form; `answer_can_use_tool_async` (feature `driver_host` or `agent_host`) takes a `tokio::sync::oneshot::Receiver`. Both fail closed with `NO_ANSWER_MESSAGE` on no receiver, timeout or dropped sender.
- #695: `Session::spawn_with(config, launcher)` where `launcher: FnOnce(Command) -> io::Result<OwnedChild>` receives the prepared command and returns the started child, so a host can confine the CLI. `Session::spawn` delegates to it. An async twin is not included: #1117's acceptance is the `Session` surface with a sandbox launcher.
- #706: Claude frame builders (`user_turn_frame`, `interrupt_frame`, `permission_*_frame`, `initialize_frame`, `set_model_frame`, `set_permission_mode_frame`) return frames without a line break; the writer adds it. Breaking for callers that appended their own. `next_request_id_with(prefix, kind)` keeps a host's id shape. `try_parse_control_response` returns a `ParseError` for a missing `response`, `request_id`, or subtype, and for an unknown subtype; `parse_control_response` is unchanged. `ClaudeAccountIdentity` reads `account` or `accountInfo` and adds `subscription_type`, `token_source`, `api_key_source` (breaking for struct literals). `InterruptReceipt` adds `pending` (the queued messages as sent). Under `UnknownPolicy::FailClosed`, a `system` message without a subtype, or an `init` without a session id, is refused; a required payload key counts only when non-null.
- #707: `rate_limit::RateLimitState` now serialises as camelCase JSON tagged by `state` and takes CodeRight's rules: observed data wins; `probe_failed` leaves `Unsupported` alone; `mark_unsupported` replaces every state, `Known` included. `driver_host::RateLimitState` delegates to it and inherits the same rules.
- #1117: `SessionExtras` gains `env_clear` and `env_allow`: an empty environment with only the allowed names inherited from this process. `Session::steer(text) -> Result<bool>`: Claude queues the text for the running turn; Codex sends `turn/steer` (`threadId`, `expectedTurnId`, `input`; unverified against a live run) and returns `false` with nothing sent when no turn is active.
- #1118: Codex maps `mcpServer/startupStatus/updated` to `AgentEvent::McpStartup`, `account/rateLimits/updated` to `AgentEvent::RateLimitWindows` (primary and secondary: used percent, window minutes, reset time), `turn/plan/updated` to `AgentEvent::PlanUpdated`. `turn/completed` and `turn/failed` emit `ResultDetail` with the failure text before `Result`. Finished `commandExecution`, `mcpToolCall`, `dynamicToolCall` and `fileChange` items emit `ToolResultContent` with their output text. Codex field names follow the pinned 0.161.0 snapshot; unverified against a live run.
- #1292: new module `tasks`: `TaskBoard` folds `AgentEvent`s (Claude `TaskCreate`, `TaskUpdate`, `TodoWrite`, `update_plan`; Codex `PlanUpdated`; subagent start and finish) into one task list with blockers and explicit `assign`. `idle_lanes()` returns `Observation::Known` only once the session has started, a task list was reported, and the host set the capacity with `set_capacity`; otherwise `Observation::Unknown`, never idle.
- `AgentEvent` gains variants `RateLimitWindows`, `McpStartup`, `PlanUpdated`; the enum is `non_exhaustive`, so exhaustive matches outside the crate need a wildcard arm.

## 0.2.3

- `find_binaries` skips empty and relative `PATH` entries, so a CLI planted in the working directory is not found (#704).
- `session::BYOK_ENV_VARS` covers the CodeRight `FORBIDDEN_BYOK_ENV_VARS` set: Bedrock, Vertex, AWS, Google, OpenAI base and org, xAI, Cursor, OpenRouter, MiniMax, TinyFish and host secrets (#705).

## Unreleased

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.2.2

- Add `agent_host` feature: modules vendor_mcp_pipe (moved from an agent host binary).

## 0.2.1 (unreleased)

- Features `agent_acp` (Agent Client Protocol types) and `driver_host` (external coding-agent CLI drivers: runtime profiles, login routes, ACP contract, harnesses) moved from CodeRight `coderight-acp` and `coderight-driver-host`.
- Sync with the product: the `agent_acp` feature is removed (nothing used it; the driver host keeps its own ACP transport). `driver_host`: Claude sign-in is not offered (`login_routes` has no `claude` route; `NativeLoginUi` and `begin_native_login` are gone from the adapter trait); vendor sign-in is time-limited (`LOGIN_TIMEOUT`, `LoginError::TimedOut`) and runs with the vendor's config-directory variable pointing at the managed home; `StdioProcessIo::capture_bounded` and `BoundedCapture` replace unbounded captures (probes time out after 10 s); vendor driver stderr goes to the log; Claude harness control frames (`ControlKind`, `ControlState`, `ControlRejection`); new `scan_claude_account_usage`. New dependency `tracing` (feature `driver_host`).

## 0.2.0 (2026-10-17)

- `discovery::run_bounded` uses `rightkit-child-process` in every feature configuration for whole-tree termination & native output readers that cancel & join after a bounded drain window. Each stream retains at most `discovery::PROBE_OUTPUT_CAP` (64 KiB); descendants retaining pipe writers cannot leave drain threads behind. Dependency minimum is `rightkit-child-process` 0.5.1.
- Breaking: `claude::answer_can_use_tool` now takes `Option<Receiver<PermissionAnswer>>` instead of an arbitrary callback. It spawns no worker; timeout/disconnect still denies. Hosts dispatch requests & own producer cancellation/cleanup. The callback wrapper was removed because safe Rust cannot cancel arbitrary callback code at a deadline.
- `session::SessionConfig::extras: SessionExtras` (new field; build with `SessionConfig::new` and assign fields). It holds `claude: ClaudeExtras`, `env_remove` and `unknown_policy`. `SessionConfig` struct literals no longer compile.
- `session::SessionConfig::scrub_byok_env` and `session::BYOK_ENV_VARS`: removes credential-class variables before spawn. The scrub is opt-in per launch; inherited variables are not removed by default. The list (`ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `OPENAI_API_KEY`, `CODEX_API_KEY`) includes two names whose CLI behavior is unverified.
- `claude::ClaudeOptions` is restored to its 0.2.0 field set, so exhaustive literals compile again. The six added options (`tools`, `no_chrome`, `fork_session`, `resume_session_at`, `max_turns`, `fallback_model`) moved to `claude::ClaudeExtras` (`#[non_exhaustive]`), used through `claude::build_args_with`.
- `codex::parse_line_with` and `CodexClient::with_policy`: under `UnknownPolicy::FailClosed`, an unknown notification in the `turn/`, `item/`, `serverRequest/` families or `error` is refused with `ParseError::unknown_required` (the client returns `ClientError::Parse`; a session emits `SessionEvent::Error` and drops the line). The family list is this crate's classification, not a Codex schema fact.
- `codex`: `turn/failed` always yields a terminal `Result` with `is_error: true` and subtype `failed`, plus `RateLimit` when the error is a usage limit. Previously a non-usage `turn/failed` was `Unknown`, and a usage-limit one was `RateLimit` only. The session also clears its active turn on `turn/failed`.

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
