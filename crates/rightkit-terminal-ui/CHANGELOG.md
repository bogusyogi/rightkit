# Changelog

## Unreleased

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-tui` (engine).
- Synced with the product TUI: `ApprovalOverlay::pending_requests` lists the open and queued approvals; `auto_approval:` requests no longer offer an allow rule; the syntax-theme root is called `theme_home` (no `CODEX_HOME`); the keyboard-enhancement opt-out variable is `RIGHTKIT_TUI_DISABLE_KEYBOARD_ENHANCEMENT`; the composer reuses the transcript's wrapping and truncation helpers and the terminal's line helpers instead of private copies.
