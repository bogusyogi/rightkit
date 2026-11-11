# Changelog

## 0.1.1

- `MemoryTier::as_str` and `MemoryTier::parse`, and `PartialEq` on `MemoryEntry`, so the memory client (`rightkit-memory-client`) uses these types instead of declaring its own copies. Additive. Taken on the recommended option of the 2026-11-11 shape review, pending the owner's confirmation.
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `cortex-core` (engine).
