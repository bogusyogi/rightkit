# Changelog

## Unreleased

- Synced with CodeRight: snapshots are session-scoped (`HistoryStore::new(workspace, state_dir, session_id)`, `fork_from` takes `state_dir`), plus redo, restore reports (`RestoreReport`), `HISTORY_UNAVAILABLE` and `rfc3339_utc_to_ns`.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-history` (engine).
