# Changelog

## Unreleased

- Removed `test_mode()` and every test branch in delivery (clipboard, keystrokes, targets, insert); delivery always takes the real OS path (Adrian, 2026-11-03: "My decision was no unit tests. Only e2e."; "dev mode, is mine ... real data. snapshot and restore.").
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `heardright-command-host` (tauri-app-next).
