# Changelog

## 0.3.0

Breaking, with no call site in this repository: `driver_host::claude_harness::HarnessSpawner` and `ClaudeHarnessSupervisor` take CodeRight's `ChildSupervisor` design (below), so `ClaudeRootRuntime::open` takes the new spawner type; `driver_host::cli_runtime::account_identity::SubscriptionAccount` gains three public fields. `rightkit-agent-server`, the only dependent that uses `driver_host`, builds none of these and names no re-typed item. The `driver_host` module is now a superset of CodeRight's private crate `coderight-driver-host` for a host that sets the options below. Nothing here was compiled or run; the gate is the first compile.

Names and persisted formats (no "CodeRight" string literal in this crate; a host that never names itself gets exactly the 0.2.8 strings):
- New `driver_host::host_names`: `mcp_server_name`, `mcp_tool_scope`, `run_scoped_policy`, `denied_vendor_subprocess_env_vars`, `forbidden_byok_env_vars`, `disable_vendor_clis_env_name`, `internal_model_id_env_name`, `gateway_token_env_name`, `exclusive_gateway_label`, `host_probe_id`, `tool_ceiling_label`, `stderr_prefix`, `display_name`, `recovery_name`, `is_default_host`. Each reads `rightkit_agent_config::{env_prefix, host_slug, app_name, env_name}` when called, never in an earlier static. For the host `CODERIGHT` (`CodeRight`): MCP server and tool scope `coderight` / `mcp__coderight`, the run-scoped policy CodeRight's own text, `CODERIGHT_DISABLE_VENDOR_CLIS`, `CODERIGHT_INTERNAL_MODEL_ID`, `exclusive_coderight_gateway`, probe ids `pos.coderight_*` (the memory probe stays `pos.rightkit_memory`), tool ceiling `coderight_mcp`, stderr prefix `coderight-driver-host`.
- The environment denied to a vendor subprocess (`LaunchSpec::env_remove` of every launch, `harness_denied_env_vars`, the Cline, Cursor, Grok and Command Code adapters, `auth::forbidden_env_var_set`, `strip_forbidden_env`, the ambient-environment tripwires) is now the constant list plus the daemon token, gateway token and driver instance secret under the host's prefix, always keeping the `RIGHTKIT_` three. It never shrinks: a `CODERIGHT` host strips `CODERIGHT_DAEMON_TOKEN`, `CODERIGHT_GATEWAY_TOKEN` and `CODERIGHT_DRIVER_INSTANCE_SECRET` as well. The constants `RUN_SCOPED_POLICY`, `DENIED_VENDOR_SUBPROCESS_ENV_VARS`, `FORBIDDEN_BYOK_ENV_VARS`, `HOST_MCP_TOOL_SCOPE` and `HOST_MCP_SERVER_NAME` stay as the default host's values.
- `DriverBinding`, `ClaudeHarnessBindingRecord` and `HarnessProgressEvent` write the host session id under `hostSessionId` (default host) or `<host slug>SessionId` (`coderightSessionId`, which is what CodeRight's files and events carry), and read both keys, so an existing CodeRight record loads unchanged. A record that carries both keys with different values is refused when read (the error names both keys); equal values are accepted, and a record with neither key is refused as before. A named host's records serialise through a JSON value, so the order of their keys follows how `serde_json` is built (derived order with the renamed key last under `preserve_order`, alphabetical without it); nothing reads the order. The Rust field stays `host_session_id`.
- `HarnessIsolationMode::label` returns `exclusive_<host slug>_gateway` (the label stored in attestation rows and hashed into the attestation address). `profile::managed_profile_root` falls back to `rightkit_agent_config::state_dir_name()` instead of `.rightkit`. Messages that said `Agent` or `Host` name `host_names::display_name` / `recovery_name`; the executor's mismatch message no longer contains `CodeRight`.

Options (`driver_host::host_options`, once per process, each setter returns `false` when already set; default = 0.2.8 behaviour):
- `set_linked_profiles(true)`: linked Claude profiles (`account_identity::{AccountSource, claude_platform_of, user_home, is_default_claude_home, read_linked_claude_identity, DiscoveredFolder, discover_claude_folders, linked_profile_home}`, `SubscriptionAccount::{source, folder, platform}` serialised only with the option on); a launch under a linked or default Claude folder sets or removes `CLAUDE_CONFIG_DIR` as CodeRight does; the persistent harness pins `HOME` only for a profile under the managed root; the sandbox home is looked up as `CLAUDE_CONFIG_DIR` before `HOME`; the Claude adapter applies the account environment on launch.
- `set_windows_job_best_effort(true)`: a Windows vendor that cannot join the engine's process-limit job runs without that cap (logged). Default: the launch fails and the tree is ended.
- `set_ignore_sdk_housekeeping_messages(true)`: the eight Agent SDK messages that carry nothing a turn depends on are skipped instead of failing the stream.
- `set_built_in_providers(ids)`: only the listed built-in providers are offered by `built_in::{iter, spec_for, seed_registry}`, `built_in_provider_ids`, `profile_layouts` and `home_layout`, the subscription account list and the vendor login (`login_routes`). CodeRight chooses `["claude"]`. Without it all six are offered.

Behaviour that follows the tools crate's options (`rightkit_agent_coding_tools` 0.2.0): with `shell::set_anchor_children_to_parent` a vendor launch registers the parent-death signal (Linux), is spawned on the anchor thread and has its process group tied to the host; with `shell::set_strip_parent_claude_session_env` the Claude launch also removes the variables of a parent Claude Code session (`CLAUDE_HOST_SESSION_ENV`).

Changes for every host (fixes or stricter):
- `StdioProcessIo` owns the vendor tree through RightKit's `OwnedCommand` on Windows too, and an adapter-backed launch passes `launch.env` through the credential guard (a managed Claude profile never gets an API key or token beside it).
- `ClaudeHarnessSupervisor` is `rightkit_job_queue::ChildSupervisor` (new dependency `rightkit-job-queue` 0.6.2, feature `supervisor`): spawn through `OwnedCommand`, monotonic generations, stop-then-boot restart, a bounded idempotent stop that reports whether the child was reaped. `claude_harness::supervisor::stdio_harness_command` builds the vendor command for the spawner.
- `ClaudeHarnessSupervisor::restart` stops the old child first and boots the next generation only when that stop reaped it (or none was running); when the old process was not reaped it returns `Err` (`cli_process_shutdown`) and spawns nothing, as 0.2.8 did. `ClaudeHarnessSupervisor::new` now returns `Result<Self, HarnessError>` (its restart policy is built with a fallible constructor; nothing panics), and `ClaudeRootRuntime::open` propagates it.
- The first process generation is 1 (0.2.8 started at 0); the binding records it as `processGeneration` on every start and restart. A stored record with `processGeneration` 0 (or any other number) still loads: the field is a plain integer that is not validated on reopen, and the next start overwrites it with the new supervisor's generation.
- The default qualification suites are functions, `qualification::default_negative_probes` and `default_positive_probes`, that read the host's names when called; the statics `DEFAULT_NEGATIVE_PROBES` and `DEFAULT_POSITIVE_PROBES` stay as the default host's 0.2.8 value. The Cline, Cursor, Grok and Command Code suites and the vendor CLI detection snapshot are built per host naming, so a host that names itself after an earlier call gets its own names.
- A host that chose `set_linked_profiles` looks up the sandbox home from `CLAUDE_CONFIG_DIR` then `HOME` only (never `CODEX_HOME` or `COMMANDCODE_HOME`), as CodeRight does; with the option off the order is unchanged.
- `AuthenticatedCliExecutor::recover_agent` logs a failed provider shutdown after a session mismatch instead of discarding it (`tracing`, now enabled by the `driver_host` feature).
- Removed the dead `agy_harness::events::result_response_text` and `agy_harness::launch::mcp_server_json` (both only had a test-only caller) and an unused import in `providers/claude.rs`.

Requirements: `rightkit-agent-config` 0.1.6, `rightkit-child-process` 0.5.17, `rightkit-job-queue` 0.6.2 (new). Not ported: tests (CodeRight's eight, listed in the lead's report) and `coderight_driver_host::init_tool_conventions` (CodeRight's own `coderight-tool-host`).

## 0.2.8

- `Session::spawn_with_child` starts a session over any `SessionChild` (a host-confined child); `SessionChild` is implemented for `OwnedChild`; `Session::spawn_with` is unchanged.

## 0.2.7 (2026-11-16)

- Requires rightkit-mcp 0.10.0 and rightkit-agent-config 0.1.3. No behaviour change.

## 0.2.6

- Breaking for dependents: a dependency requirement was raised to a semver-breaking version.

## 0.2.5 (unpublished)

- Requires `rightkit-agent-engine-protocol` 0.2.0, whose approval records carry the approvals core's `Binding` (mandatory digest) instead of an optional `payload_digest`. Decision 4 of the 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/README.md, "Agent engine") taken on the recommended option, pending the owner's confirmation; design note: docs/audits/2026-11-11-shape-matrix/reports/agent-engine-contract-design.md.

## 0.2.4

Wave 2 batch w2-01 (CodeRight port): issues #567, #571, #695, #706, #707, #1117, #1118, #1292.
Source-level only: nothing here was compiled or run; the gate is the first compile.

- Build fixes: `event.rs` imports `BTreeMap` and `rate_limit.rs` imports `RateLimitInfo`; `claude.rs` imports `Receiver` for `answer_can_use_tool`. These were missing at 0.2.3 (#571).
- Build fix: `vendor_mcp_pipe::VendorMcpPipe::start` takes a new `vendor_mcp_pipe::VendorMcpHandler` trait instead of the unnameable `rightkit_mcp::mcp_server::McpStdioServer`; the host implements the trait for its MCP server. Feature `agent_host` now enables `driver_host`, which `VendorMcpPipe::attach` needs for `LaunchSpec`.
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
