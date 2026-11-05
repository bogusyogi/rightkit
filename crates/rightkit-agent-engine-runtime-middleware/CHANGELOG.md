# Changelog

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- Synced with CodeRight: the store file is rewritten at most once per second (`RuntimeMiddlewareRegistry::flush` writes held-back records), is compact JSON, and never exceeds 1 MiB (oldest spans and observations are dropped until it fits).
- First RightKit release, moved from `coderight-runtime-middleware` (engine).
