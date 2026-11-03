# Changelog

## Unreleased

- `paths::resolve` no longer calls `Options::qa_overrides` and `Options::test_root`, which rightkit-fs 0.4.11 removed with the test-mode residue.
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `membrane-runtime` (engine).
