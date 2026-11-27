# Changelog

## 0.2.0

Upstreamed from CodeRight's private engine copy, in three parts (the turn loop and session controls; tool dispatch, approval and verification; compaction, transcript, usage and the shared files). A host that does nothing keeps the pre-0.2.0 behaviour except for the items under "Applies to every host". Every choice that is not a fix or strictly stricter sits behind a once-per-process option in the new `host_options` module (`set_<name>(true)`, first call wins, `false` until set, documented there); a CodeRight host sets the options listed at the end.

### Breaking (a host that builds these by hand, matches them exhaustively or names them)

- `LlmStreamEvent` gains `UsageCost`, `RateLimit` and `ToolArgumentsPartial`; an exhaustive `match` needs three arms.
- `LlmRequestMetadata` gains `cache_retention: Option<CacheRetention>` (build it with `..Default::default()`).
- `CompactionCommandV1` gains `instructions: Option<String>` (the `/compact <text>` focus; `None` for an automatic trigger and for a manual compaction without text).
- `compactor_breaker::MAX_AUTOCOMPACT_FAILURES_ENV` and `compactor_output_budget::MODEL_MAX_OUTPUT_TOKENS_ENV` become `MAX_AUTOCOMPACT_FAILURES_ENV_SUFFIX` and `MODEL_MAX_OUTPUT_TOKENS_ENV_SUFFIX`: the variable names are `rightkit_agent_config::env_name(<suffix>)`, so a host that names itself reads `<PREFIX>_MAX_AUTOCOMPACT_FAILURES`, and a host that does not reads the same `RIGHTKIT_...` names as before.
- `adapt_consumer::APPLICATION_RECEIPT_DOMAIN` (a constant, `"coderight.adapt_consumer.application_receipt.v1"`) becomes `application_receipt_domain()`, `<host>.adapt_consumer.application_receipt.v1`. A host that has not named itself now derives receipt ids with `rightkit.` where it derived them with `coderight.`; a receipt id stored by an earlier version no longer equals the id derived for the same proposal, so a replay of an already-applied proposal is processed again instead of short-circuiting to the cached receipt. A host that names itself `coderight` derives today's ids.
- `IterationBudget::refund(amount, reason)` loses `reason`; `IterationBudget::{remaining, audit_trail}` and `BudgetEvent` are removed (the audit trail was never read).
- `LoopDetectorConfig` gains `tool_await_secs: Option<u64>` (`[turn] tool-timeout-secs`; `None` keeps the deadline derived from the turn's iteration budget). `DecayingObservationProjection` gains `protect_tokens` and `prune_min_tokens` (set by `new`). Build both with their constructors or `Default`.
- Requires `rightkit-agent-config` 0.1.5 and `rightkit-hooks` 0.3.1 (new dependency edge to `rightkit-agent-config`; new dependency `serde_yaml` 0.9). `qualification` now forwards `rightkit-agent-coding-tools/qualification`; the post-effect crash fault point (EVT006) is compiled only with `host` and `qualification`, it used to need `host` alone.
- Log targets are `rightkit_turn`, `rightkit_provider`, `rightkit_hook` and `rightkit_tool` (CodeRight's were `coderight_*`): a `RUST_LOG` filter names them.

### New

- Public: `host_options` (21 options, below), `CacheRetention` and `LlmRequestMetadata::cache_retention`, `LlmClient::message_origin`, `LlmStreamEvent::{UsageCost, RateLimit, ToolArgumentsPartial}`, `ModelPrices`, `UsageAccount::{use_prices, set_long_context_tier, record_with_retention, bill_last_record, cost_is_billed, cost_source, cost_is_known}` and `ONE_HOUR_CACHE_WRITE_FACTOR` (a provider-billed request prices at the provider's own charge and counts as known cost for `exceeds_budget`; default accounting is unchanged until a host calls them), `Transcript::{push_nudge, push_nudge_at, tag_last_assistant_origin, pop_reasoning_only_assistant}`, `transcript::{is_nudge, loop_warning_nudge, LOOP_WARNING_NUDGE_FIELD}`, `FallibleSummarizer::set_instructions`, `CompactionModelPolicy::with_user_limits`, `compaction_policy::is_silent_usage_overflow`, `Conduit::durable_settled`, `WireProjection::project_with_window`, `DecayingObservationProjection::with_token_window`, `image_eviction::without_inline_image_payloads`, `cache::fnv1a_64`, `GOAL_REMINDER_PREFIX`, `TURN_LIMIT_OBSERVATION_KIND`, `agents::website_designer_system_prompt()` (the designer prompt with the host's product name and state folder; the constant stays), `admission_trace::{stages, reached_execute, canonically_ordered}` (read access to the dispatch seam's admission stages), and the re-exports `failure::{is_retryable_stream_failure, stream_retry_delay}` at the crate root with `classify_failure` and `FailureKind`.
- An approval card carries the effects of the call (`effects`), and the engine records a repeated-call observation and an `allowed` policy event for an allow that came from an explicit rule; the added fields are optional and unset for a client that does not read them.
- `<PREFIX>_VERIFY_PATH` overrides the search path the verifiers resolve their programs on.
- Structured-config checks (JSON with the comment- and trailing-comma-tolerant form for `tsconfig*`, `jsconfig*`, `.eslintrc*` and `.vscode/`, TOML, YAML) on a file an edit just changed: the edit result reports a file that no longer parses (`post_edit_diagnostics`, private).

### Behind options (default off, the pre-0.2.0 behaviour)

Turn loop: `hook_errors_as_tracing` (a non-blocking hook error is a `tracing` event on `rightkit_hook`, not a line on standard error), `completion_repair_cap` (the completion gate's refusals are bounded at eight repairs per turn, then the turn finishes with a warning: the looser setting), `reset_loop_counters_per_turn` (a new user message resets the stall and idempotency counters), `loop_progress_resets_repeats` (a repeated call that returned something new counts as progress).

Tool dispatch and approval: `wide_read_tool_inline_limit` (read/search/list/sub-agent results stay inline up to 32 KB instead of 4,000 bytes), `inline_file_diagnostics` (an edit result carries the language server's diagnostics for the files it changed), `abac_allow_authorizes` (an ABAC allow is a positive authorization, like a permission rule: the looser setting), `mcp_auth_unsupervised` (`mcp_auth` is not cut off by the turn-budget deadline), `mcp_read_only_hint_concurrent` (an `mcp_call` the server marked `readOnlyHint` runs beside other reads, skipping the serial path's replay suppression, mutation-diagnostics transaction and reviewer record: the looser setting).

Verification: `trimmed_builtin_verifiers` (the built-in Rust verifiers are `cargo check`, `cargo fmt --check` and `cargo metadata`, without clippy and `cargo test --workspace --lib`), `scope_cargo_to_touched_packages` (`cargo check` and `cargo fmt` run on the touched packages, `-p <package>`), `config_error_predating_edit_not_reported` (a config file that already failed the same way before the edit is not reported, in the edit result or in the verification).

Compaction: `compaction_input_budget`, `compaction_tail_byte_budget`, `compaction_carry_prior_summary`, `compaction_newest_evidence_first`, `bound_obligation_results`, `pre_compaction_tool_clearing` (with `DecayingObservationProjection::with_token_window`, `protect_tokens` and `prune_min_tokens`; the host's `TOOL_PROTECT_TOKENS` and `TOOL_PRUNE_MIN_TOKENS` environment variables), `compaction_full_reserve_trigger` (context occupancy counts input only, the reserve boundary sits a full reserve below the send gate, the prefire boundary a fixed lead below the trigger; also changes `PostCallEnvelope::context_tokens` and `UsageAccount::context_tokens`; the default keeps completion tokens as context and the margin `min(window / 16, 16384)`), `compaction_flat_image_tokens` (the trigger counts each inline image at a flat 765 tokens, the default estimates the base64 text at its length and adds the flat cost only when the measured context is below that estimate), and `todo_write_result_trusted` (a `todo_write` result is not wrapped as untrusted data: the looser setting).

### Applies to every host

Stricter or fixes, unconditional:
- Secret redaction no longer restores a secret into the arguments of a web, browser or MCP tool; the admission trace is bounded at 16,384 entries; the output the engine keeps of a blocked call is bounded; an unconditional ABAC allow for the shell tool still passes the command classifier outside Bypass; an MCP deny rule matches the qualified tool name too; a hook-serial rule applies only with a `PreToolUse` hook configured and fails closed on an invalid hook configuration; a failure is classified by HTTP status first.
- The loop detector's hard stop is checked before dispatch; output-limit, empty-reply and stream retries are bounded; a usage-reported full window compacts before the next request; hook context and the gate text are bounded; the optional-verifier note is shown once; a "model capacity unavailable" refusal no longer triggers compaction.
- Compaction: a preserved tool output leaves compaction as a pinned user message quoting it as untrusted data (not a system message), and is not counted as a user turn; observation masking applies only before the retained tail (it masked by user-turn count as if it were a message count); a tool result's trust envelope is keyed by the call's tool name, as the durable event records it, so a resumed session sends the same bytes (a full prompt-cache miss before); a provider that reuses a tool-call id across turns has the repeat of an earlier call dropped and a different call re-keyed (`<id>~<n>`) and run; the prompt-cache break detector measures idle gaps send to send, treats a gap past the requested lifetime and a model switch as expected, and reports the break; the masking marker keeps a one-line preview (up to 120 characters of the output's first line) and the stored path; the compaction summarizer's request carries `CacheRetention::None`; `UsageAccount` prices a provider-billed request at the provider's charge, applies the long-prompt tier and the one-hour cache-write rate.
- Behaviour that was serial now overlaps, with every permission, approval and assurance check unchanged: read-only calls run concurrently unless a `PreToolUse` hook is configured (before: whenever a hook bus existed, which is every session), and consecutive read-only calls streamed in one reply run as one batch.
- Harness messages (nudges, loop warnings) are pushed with `push_nudge`; a pending steer between calls stops the remaining calls; the assistant's interim or final completion event is emitted once the turn's fate is known; `PostToolBatch` and `FileChanged` hooks run; `Conduit::durable_settled` is awaited every iteration.

Naming (default output unchanged): the state folder is `rightkit_agent_config::state_dir_name()` (the restore-point store), summaries and prompts use `app_name()`, receipt issuer ids are `host_issuer()`; what a host cannot set: the `__cr_bridge` and `__cr_tools` globals of the code-mode JavaScript sandbox, and the `RIGHTKIT_HOOK_*` names of the hook variables (the hooks crate maps them to the host's prefix).

Lint cleanup: removed the `let _ =` discards (conduit, engine_composition, engine_loop, engine_session_controls, lineage_budget, engine_loop_dispatch, assurance and approval paths), the `unused_mut` waiver in the compactor, the `unused_imports` waiver in `engine.rs`, the `_reserve` binding in `compaction_policy`, the write-only `closed_ledger_digest` field of the assurance broker, and the `deprecated` waiver on `cache::fnv1a` (the legacy hash is spelled out in the crate; its values are unchanged). Dead code removed with what only it reached: `diagnostic_infrastructure_failures`, the iteration budget audit trail, the dead fields of `GoalContinuationUpdate`.

### Options a CodeRight host sets (`host_options::set_*(true)`), with the sibling crates' own setters

`hook_errors_as_tracing`, `completion_repair_cap`, `reset_loop_counters_per_turn`, `loop_progress_resets_repeats`, `wide_read_tool_inline_limit`, `inline_file_diagnostics`, `abac_allow_authorizes`, `mcp_auth_unsupervised`, `mcp_read_only_hint_concurrent`, `trimmed_builtin_verifiers`, `scope_cargo_to_touched_packages`, `config_error_predating_edit_not_reported`, `compaction_input_budget`, `compaction_tail_byte_budget`, `compaction_carry_prior_summary`, `compaction_newest_evidence_first`, `bound_obligation_results`, `pre_compaction_tool_clearing`, `compaction_full_reserve_trigger`, `compaction_flat_image_tokens`, `todo_write_result_trusted`; and `rightkit_agent_coding_tools::tool_options::set_edit_test_file_additions(true)`.

### Not changed

`compaction_policy::reserve_boundary` collapses to zero when `reserve + min(window / 16, 16384) >= window` (every turn then compacts until the no-gain stop; `with_user_limits` keeps the reserve under half the window). Neither this nor the prefire boundary equal to the trigger when the reserve boundary binds is changed under the default; `compaction_full_reserve_trigger` has neither problem.

## 0.1.4

- Requires `rightkit-hooks` 0.3.0 (the deprecated destructive guard is removed). No behaviour change.

## 0.1.3 (2026-11-16)

- Breaking for a host outside this repository that took `default-features = false` and relied on the implied OS keyring: the `rightkit-mcp` dependency no longer names `agent_mcp-os-keyring`, so a build without `host` no longer links the OS credential store (`rightkit-credentials`). New feature `os-keyring` (`rightkit-agent-coding-tools/os-keyring`, `rightkit-mcp/agent_mcp-os-keyring`, `rightkit-hooks/agent_hooks-os-keyring`); `host` implies it, so a default-features user is unchanged.
- Requires rightkit-agent-coding-tools 0.1.8 and rightkit-hooks 0.2.11.
- Lint clean without `host`: the Sentinel launch imports, the command-verifier output parsers (`parse_compiler_line` and its helpers, `parse_lsp_finding_line`), `interpreter_read_roots` and the interactive-terminal process kind are compiled only under the `host` feature, the one configuration that uses them, and two `return` statements that only existed for the host-off branch became tail expressions. `cargo clippy --no-default-features --all-targets -- -D warnings` (the build an iOS host links through rightkit-agent-engine-mobile-runtime) now passes. All gated items are crate-private; no public item, feature or behaviour changes.

## 0.1.2 (2026-11-16)

- Requires rightkit-mcp 0.10.0. Names `agent_mcp-os-keyring` explicitly, so the build is unchanged. No behaviour change.
- The package no longer ships `src/steering.rs` (the live steering queue is `api_sessions/steering.rs` in rightkit-agent-server) and `src/evidence_replay.rs`, neither of which `src/lib.rs` declared nor anything called. No public item, feature or behaviour changes; the compiled library is unchanged.

## 0.1.1 (unpublished)

- Requires `rightkit-mcp` 0.9.0 (agent MCP server options, bearer config, prompts and the CodeRight sync; see its changelog).
- Builds against `rightkit-agent-engine-protocol` 0.4.0: the events and approval records it emits leave the new optional fields unset (`effects`, `cost_billed`, `ttft_ms`, `total_ms`, `rate_limit`, `cache_ttl_ms`, `PendingApprovalContext` flat-shape fields), and `seq_of` handles `ToolArgumentsPartial` and `SubAgentOutput`. No behaviour change.

- A tool call that waits for approval is bound by the approvals core's rule: its `PendingApprovalContext` carries
  a mandatory `rightkit_approvals::Binding` (the effect payload digest of the exact final payload, and the
  destination: the call's network host, else the tool) built with `rightkit_agent_engine_protocol::effect_binding`,
  instead of an optional `payload_digest`. Requires `rightkit-agent-engine-protocol` 0.2.0. Decision 4 of the
  2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/README.md, "Agent engine") taken on the
  recommended option, pending the owner's confirmation; design note:
  docs/audits/2026-11-11-shape-matrix/reports/agent-engine-contract-design.md.

## Unreleased

- Builds against the protocol's new `origin` and `phase` fields: the assistant messages and completion events the engine emits carry `origin: None` and `phase: final`. No behaviour change.

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-engine` (CodeRight).
- Skills come from the host: skill triggers, hints and effect enforcement take the skills a host-installed `rightkit_agent_host_traits::SkillCatalog` reports as triggered; with no catalog installed no skill triggers.
- Synced with CodeRight: approvals wait with no timer and are denied at once when no human client is attached (`Conduit::subscribe_human`, `unattended` / `detached` / `cancelled` reasons); the graduated stall ladder is replaced by a `repeated_call` Ask on the fourth identical call; `ToolPolicyEvent` records for hook blocks and sandbox refusals; session-scoped history snapshots before write tools; `PreModelRequest` / `PostModelRequest` / `PostCompact` / `Interrupt` / `PermissionRequest` hooks; `tool_search` no longer taints content provenance; test-file protection spares new files; auto-approval cache removed; bounded gate and verifier text; `SessionEngine::set_os_sandbox_enforced`; rewind keeps the removed tail for redo.
