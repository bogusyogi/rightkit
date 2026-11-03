# Changelog

## 0.1.2 (unreleased)

- Removed test-only code: `ApprovalModal::arm_for_tests` (Adrian, 2026-11-03: "make sure no debug assertions in their respective apps/ repos"; "My decision was no unit tests. Only e2e."; "dev mode, is mine. im the fucking owner, my data. no dummy data ... real data. snapshot and restore."). Release behaviour unchanged.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-tui` (engine).
- Synced with the product TUI: `ApprovalOverlay::pending_requests` lists the open and queued approvals; `auto_approval:` requests no longer offer an allow rule; the syntax-theme root is called `theme_home` (no `CODEX_HOME`); the keyboard-enhancement opt-out variable is `RIGHTKIT_TUI_DISABLE_KEYBOARD_ENHANCEMENT`; the composer reuses the transcript's wrapping and truncation helpers and the terminal's line helpers instead of private copies.
