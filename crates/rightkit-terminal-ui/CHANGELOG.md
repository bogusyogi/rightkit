# Changelog

## 0.1.3 (2026-11-16)

- Fix: a long answer written into the terminal's scrollback now wraps every row to the terminal width. A link longer than the terminal (a bare URL that carries an OSC 8 link, or a linked label) was written as one over-wide row that the terminal soft-wrapped on its own, so the row count the scrollback insert had counted was wrong and lines were pushed out of place. A list nested so deep that its continuation indent took half the terminal or more left a column or two for text and put one character on each row; such a line now continues from the left margin. Plain bare URLs without a link still stay whole so the terminal can match them as one link. Ported from the product TUI (CodeRight, Wave 25) where the fix was made first.
- Removed dead source files that no module declared and nothing read: `src/composer/wrapping.rs`, `src/composer/line_truncation.rs` and `src/composer/line_utils.rs` (the composer re-exports the transcript's wrapping and truncation and the terminal's line helpers), and the 97 `.snap` snapshot files under `src/` (there is no snapshot test target). In the transcript wrapping helpers, `adaptive_wrap_line_to_width` replaces the now unused crate-internal `adaptive_wrap_line`. No other behaviour changes.

## 0.1.2 (unreleased)

- Removed test-only code: `ApprovalModal::arm_for_tests` (Adrian, 2026-11-03: "make sure no debug assertions in their respective apps/ repos"; "My decision was no unit tests. Only e2e."; "dev mode, is mine. im the fucking owner, my data. no dummy data ... real data. snapshot and restore."). Release behaviour unchanged.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-tui` (engine).
- Synced with the product TUI: `ApprovalOverlay::pending_requests` lists the open and queued approvals; `auto_approval:` requests no longer offer an allow rule; the syntax-theme root is called `theme_home` (no `CODEX_HOME`); the keyboard-enhancement opt-out variable is `RIGHTKIT_TUI_DISABLE_KEYBOARD_ENHANCEMENT`; the composer reuses the transcript's wrapping and truncation helpers and the terminal's line helpers instead of private copies.
