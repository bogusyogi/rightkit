# Changelog

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
