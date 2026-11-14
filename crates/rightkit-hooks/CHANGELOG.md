# Changelog

## Unreleased

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
