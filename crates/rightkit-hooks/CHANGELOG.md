# Changelog

## 0.2.0 (2026-10-17)

- `compose_reply` keeps a notice-only reply: a `systemMessage` with no decision and no context prints `{"systemMessage": ...}` instead of nothing.
- `ReceiptStore::membrane(root)` is the store Membrane reads and writes. Receipt files carry Membrane's `harness` and `state` (`emitted`/`delivered`) next to this crate's `wire`, `level` and `chars`; reads accept either spelling, so both sides share one `receipts/<session>/<id>.json` store.
- New `membrane` module: Membrane's hook wire types (`HookModuleId`, `HookModuleOutputV1`, `HookModuleResultV1`, `HookDispatchResultV1`, `HookSpecificOutputV1`, `HookHostResponseV1`) and `project_hook_host_response`, with the shipped JSON shapes.
- `EventKind` implements `Serialize` and `Deserialize` as its host event name.

## 0.1.0

- First release: typed hook payload parsing for Claude Code and Codex, per-event output builders, a dispatcher with per-module and total deadlines that exits 0 on failure, and emitted/observed delivery receipts with a transcript-scan module.
