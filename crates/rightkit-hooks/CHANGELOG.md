# Changelog

## 0.2.3 (unpublished)

- Issue #1041: `ReceiptStore::list_all()` lists receipts of every session; `ReceiptStore::mark_observed(id)` flips the `emitted` receipts with that id in any session and returns every receipt with the id.
- Issue #1041: caller-named receipt ids of 1 to 128 characters of `[A-Za-z0-9_-]` (`valid_receipt_id`); generated ids stay alphanumeric. `observe` now sees these receipts.
- Issue #421: `HookEvent::from_stdin(wire)` and `HookEvent::from_reader_deadline(wire, reader, first_byte, quiet)` read a hook payload under a first-byte deadline (2 s) and a quiet-gap deadline (400 ms) on a helper thread, with `STDIN_FIRST_BYTE` and `STDIN_QUIET`; a stalled host no longer blocks the hook.
- Issue #1041: new `HookSeen` per-session heartbeat store (`write`, `read`, `newest`), owner-only atomic writes, pruned by age (14 days) and count (1000 files), with `with_limits` for other limits.

## 0.2.2 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.2.1 (unreleased)

- Feature `agent_hooks`: agent lifecycle hook runner and hook protocol moved from CodeRight `coderight-hooks`.

## 0.2.0 (2026-10-17)

- `compose_reply` keeps a notice-only reply: a `systemMessage` with no decision and no context prints `{"systemMessage": ...}` instead of nothing.
- `ReceiptStore::membrane(root)` is the store Membrane reads and writes. Receipt files carry Membrane's `harness` and `state` (`emitted`/`delivered`) next to this crate's `wire`, `level` and `chars`; reads accept either spelling, so both sides share one `receipts/<session>/<id>.json` store.
- New `membrane` module: Membrane's hook wire types (`HookModuleId`, `HookModuleOutputV1`, `HookModuleResultV1`, `HookDispatchResultV1`, `HookSpecificOutputV1`, `HookHostResponseV1`) and `project_hook_host_response`, with the shipped JSON shapes.
- `EventKind` implements `Serialize` and `Deserialize` as its host event name.

## 0.1.0

- First release: typed hook payload parsing for Claude Code and Codex, per-event output builders, a dispatcher with per-module and total deadlines that exits 0 on failure, and emitted/observed delivery receipts with a transcript-scan module.
