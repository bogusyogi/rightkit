# Changelog

## 0.2.0 — 2026-10-07

Breaking:
- Every suite token is now `--rk-*` (`--rk-ink`, `--rk-accent`, `--rk-plane`, ...). shell.css no longer defines unprefixed `:root` variables, so it cannot recolour an app that uses names like `--ink`. Apps that relied on the flat names can opt in with `@rightkit/app-shell/suite-aliases.css`.
- Wordmark font is `--rk-wordmark-font` (was `--rk-wordmark`).

Fixes:
- Primary/danger buttons inside the shell keep their own text colour: the shell's button reset is `:where(.rk-shell) button` (0,0,1), below every `.rk-btn--*` rule.

Added:
- `FuseCurves` works outside `.rk-item` and without shell.css (inline positioning, `fill` and `size` props; `--rk-fuse-fill`).
