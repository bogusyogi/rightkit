# Changelog

## 0.3.1

Additive; no existing item changes. `agent_hooks` (feature `agent_hooks`) gains the process-wide bookkeeping the engine uses to keep a short-lived host alive until its fire-and-forget hooks have run, ported from CodeRight's hooks shim:

- `track_pending(JoinHandle<()>)` remembers a fire-and-forget hook task (a `SessionEnd` hook, which may not block a turn); finished tasks are dropped from the list as new ones arrive.
- `wait_pending(Duration)` waits up to the limit for every tracked task. A task that ended abnormally, and a wait that ran out of time, are reported on standard error (CodeRight's shim stayed silent in both cases).
- `note_session_start()` and `note_session_end()` count the turns this process's engine started and ended; `wait_session_ends(Duration)` waits up to the limit until every started turn has recorded its end (a turn that runs in another process counts nothing here). Same standard-error report when the wait runs out.

`wait_pending` and `wait_session_ends` are added with the three recorders because the state lives beside them: a host cannot read it from anywhere else. Nothing runs unless the engine (`rightkit-agent-engine-core` 0.2.0) or a host calls them.

## 0.3.0

- Removed the deprecated substring `DestructiveGuard` and `DestructiveGuardDecision` (and the public `guard` module); use `rightkit_agent_policy::catastrophic_reason` (rightkit#1687).

## 0.2.13

- Deprecated `agent_hooks::DestructiveGuard` and `DestructiveGuardDecision` (rightkit#1687). `rightkit-agent-policy` is RightKit's destructive-command guard: it parses the command (groups, `sh -c` bodies, substitutions, wrappers), while this guard only matches substrings, so it misses `(cd / && rm -rf -- *)` and flags `echo "rm -rf"`. Nothing in RightKit decides with it. Behaviour is unchanged; the types are removed in the next minor release. Use `rightkit_agent_policy::catastrophic_reason` and the host's permission engine.
- New end-to-end test `tests/deprecated_guard_e2e.rs` (feature `agent_hooks`): the deprecated guard still blocks, asks and allows exactly as in 0.2.12.

## 0.2.12

- Fixed: with a renamed environment prefix (`set_env_prefix`), a variable the host passes under its own prefix is no longer prefixed twice. `run_event_hooks(..., &[("CODERIGHT_HOOK_ROLE", ..)])` used to reach the hook as `CODERIGHTCODERIGHT_HOOK_ROLE`, so the hook never saw it (CodeRight renamed every extra to `RIGHTKIT_HOOK_*` to work around it). A name already carrying the host's prefix and an underscore is kept; a `RIGHTKIT_` name is renamed as before. Covers command hooks and `url` hook bodies alike.
- New end-to-end test `tests/host_prefix_env_e2e.rs` (Unix, feature `agent_hooks-host-env`): a `SessionEnd` command hook records its environment; both spellings arrive once as `CODERIGHT_HOOK_*`. Fails on 0.2.11, passes here. New dev-dependency `tokio` (macros, multi-thread runtime) for the test.

## 0.2.11 (2026-11-16)

- Breaking for a host outside this repository that relied on the implied OS keyring: with `agent_hooks` the crate no longer names `rightkit-llm`'s `agent_providers-os-keyring`, so the OS credential store (`rightkit-credentials`) is no longer linked. New feature `agent_hooks-os-keyring` adds it (implies `agent_hooks`); a host that needs the keyring names it. Nothing in the crate's own code needs the keyring, so no item changes.
- Removed four source files under `src/guard/` (4,480 lines) that no module declared, so they were never compiled but were shipped in the package. They were a host's hook-binary source and are not part of this crate; `agent_hooks::guard` (the destructive-action guard) is a different file and is unchanged. No compiled code changes.

## 0.2.10 (2026-11-16)

- Requires rightkit-llm 0.7.0. Names `agent_providers-os-keyring` explicitly, so the build is unchanged. No behaviour change.

## 0.2.9

- Breaking for dependents: a dependency requirement was raised to a semver-breaking version.

## 0.2.8

- Depends on `rightkit-agent-engine-protocol` 0.4.0 (additive events and fields, see its changelog).
- Synced with CodeRight's newer hook runner (CodeRight `crates/hooks`), so CodeRight can use `agent_hooks` instead of its copy; `set_env_prefix("CODERIGHT")` and `set_state_dir_name(".coderight")` give its `CODERIGHT_*` variables and `.coderight/hooks.json`.
- `HookBus::set_revocation_filter` with `HookRevocationFilter`: the host's live test of whether an identified hook is revoked. `specs_for` (so `PreToolUse` and every chain event) skips a revoked hook, identified as `hooks_for` identifies it. Without a filter nothing is skipped, as before.
- `HookBus::set_url_sink` with `HookUrlSink`: the host's sender for `url` hooks of chain-fired events (events the bus does not publish); without a sink nothing is posted.
- `HookBus::rewrite_post_tool_result` runs `PostToolUse` command hooks that declare `"rewrite": true` (`updatedOutput`, `additionalContext`); `HookBus::run_blocking_stop_event_for_role` passes the sub-agent role in `RIGHTKIT_HOOK_ROLE` (host prefix applied).
- Chain: `HookDecision::Defer` (`"decision": "defer"`, reported as an `ask` starting `deferred by <event> hook`), `HookResult::updated_output`, `HookSpec::rewrite` and `block_on_failure` (`"onFailure": "block"`, with `with_rewrite` / `with_block_on_failure`), new event names (`PostToolBatch`, `PermissionDenied`, `PreModelSwitch`, `PostModelSwitch`, `DirectoryAdded`, `FileChanged`). Breaking for exhaustive matches on `HookDecision` and for `HookSpec` built as a struct literal.
- Safety fixes taken from CodeRight: a fail-closed event denies on a hook that could not run (shell exit 126, 127 or 9009), on exit-0 stdout cut at the 256 KiB capture limit that opens a JSON object but does not parse, and on tool arguments too large for the environment (`E2BIG`) instead of passing them truncated or not at all; stdout that is not exactly one JSON document keeps only the deny, ask, defer or stop of its first JSON value; a lagging bus subscriber reports the dropped events on stderr.
- `dry_run_hook_with_adapter` honours the hook's own timeout, bounds each captured stream at 256 KiB and drains the pipes for at most two seconds after the hook exits (before: a fixed 30 seconds and unbounded reads).
- Unit tests are not ported (end-to-end tests only).

## 0.2.7

- Breaking for dependents: a dependency requirement was raised to a semver-breaking version.

## 0.2.6

- Host naming options, defaults unchanged: `agent_hooks::set_env_prefix` / `env_prefix()` (default `RIGHTKIT`: the `<prefix>_HOOKS_JSON` variable read, and every `<prefix>_HOOK_*` variable a hook command receives, including `_REASON`) and `set_state_dir_name` / `state_dir_name()` (default `.rightkit`; `chain::project_hook_file()` is the project hook file). `PROJECT_HOOK_FILE` and `HOOKS_JSON_ENV` remain the defaults. Call once at start-up.

## 0.2.5 (unpublished)

- Requires `rightkit-agent-engine-protocol` 0.2.0, whose approval records carry the approvals core's `Binding` (mandatory digest) instead of an optional `payload_digest`. Decision 4 of the 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/README.md, "Agent engine") taken on the recommended option, pending the owner's confirmation; design note: docs/audits/2026-11-11-shape-matrix/reports/agent-engine-contract-design.md.

## 0.2.4

- Publishes the source changes made since 0.2.3 was published under the same version number (src/agent_hooks/bus.rs, src/agent_hooks/chain.rs, src/agent_hooks/mod.rs, src/agent_hooks/registry.rs).

## 0.2.3 (unpublished)

- Issue #1041: `ReceiptStore::list_all()` lists receipts of every session; `ReceiptStore::mark_observed(id)` flips the `emitted` receipts with that id in any session and returns every receipt with the id.
- Issue #1041: caller-named receipt ids of 1 to 128 characters of `[A-Za-z0-9_-]` (`valid_receipt_id`); generated ids stay alphanumeric. `observe` now sees these receipts.
- Issue #421: `HookEvent::from_stdin(wire)` and `HookEvent::from_reader_deadline(wire, reader, first_byte, quiet)` read a hook payload under a first-byte deadline (2 s) and a quiet-gap deadline (400 ms) on a helper thread, with `STDIN_FIRST_BYTE` and `STDIN_QUIET`; a stalled host no longer blocks the hook.
- Issue #1041: new `HookSeen` per-session heartbeat store (`write`, `read`, `newest`), owner-only atomic writes, pruned by age (14 days) and count (1000 files), with `with_limits` for other limits.

## 0.2.2 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.2.1 (unreleased)

- Feature `agent_hooks`: agent lifecycle hook runner and hook protocol moved from CodeRight `coderight-hooks`.
- `agent_hooks` sync: hooks come from the `[hooks]` config key (`HookBus::set_configured_hooks`, `ConfiguredHooks`), the project hook file (`.rightkit/hooks.json`, trusted workspaces only, `load_project_hook_file`) and `RIGHTKIT_HOOKS_JSON`; the per-event `RIGHTKIT_HOOK_*` command variables are gone. New events `PostToolUseFailure`, `PermissionRequest`, `PreModelRequest`, `PostModelRequest`, `PostCompact`, `SubagentStart`, `Interrupt` and `PreCommit` in `HOOK_EVENT_NAMES`; `HookKind` (`command`, `url`, `mcp`); an unknown event name in a hooks document is an error; hooks that cannot start are reported in `ChainReport::unrunnable` and a failing `PreCommit` gate blocks; no credential variable reaches a hook process (needs `rightkit-llm`); hooks have ids (`<Event>#<n>`, `IdentifiedHook`, `HookBus::hooks_for`, `inventory`).

## 0.2.0 (2026-10-17)

- `compose_reply` keeps a notice-only reply: a `systemMessage` with no decision and no context prints `{"systemMessage": ...}` instead of nothing.
- `ReceiptStore::membrane(root)` is the store Membrane reads and writes. Receipt files carry Membrane's `harness` and `state` (`emitted`/`delivered`) next to this crate's `wire`, `level` and `chars`; reads accept either spelling, so both sides share one `receipts/<session>/<id>.json` store.
- New `membrane` module: Membrane's hook wire types (`HookModuleId`, `HookModuleOutputV1`, `HookModuleResultV1`, `HookDispatchResultV1`, `HookSpecificOutputV1`, `HookHostResponseV1`) and `project_hook_host_response`, with the shipped JSON shapes.
- `EventKind` implements `Serialize` and `Deserialize` as its host event name.

## 0.1.0

- First release: typed hook payload parsing for Claude Code and Codex, per-event output builders, a dispatcher with per-module and total deadlines that exits 0 on failure, and emitted/observed delivery receipts with a transcript-scan module.
