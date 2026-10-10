# Changelog

## 0.2.2 — 2026-10-10

- Test-only: the real-browser e2e harness imports `@rightkit/qa-chrome` directly; the devDependency moves from the `@rightkit/qa` facade (`@rightkit/qa/chrome` re-exports it). No runtime change.

## 0.2.1 — 2026-10-07

- Test-only: the real-browser e2e harness launches Chrome for Testing through `@rightkit/qa/chrome` (`RIGHTKIT_CHROME_FOR_TESTING` overrides; skips with install instructions when missing, never regular Chrome) and closes gracefully (CDP `Browser.close`, SIGTERM, SIGKILL last) with its temp profile removed. No runtime change.

## 0.2.0 — 2026-10-07

Breaking:
- Token overrides are emitted as `--rk-<name>` to match @rightkit/app-shell 0.2.0.
- `wordmark` is the product name as written (informational, never emitted). The font stack moved to `wordmarkFont`, emitted as `--rk-wordmark-font`.
