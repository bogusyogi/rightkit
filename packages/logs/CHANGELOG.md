# Changelog

## 0.1.6 — unpublished staging

- Add opt-in `createDiagnosticsRecorder`: 80-entry persistent renderer history with static event allowlists, content-free projection, bounded legacy migration, dedup, lifecycle flushes, privacy toggling, & explicit persistence results.
- Document composition with existing crash capture through one app privacy gate for recorder & native forwarding.
- Preserve structured crash codes & readable rejection reasons with bounded, cycle-safe private formatting, guarded property access, & intact-value scrubbing before truncation.
- Tests: `crates/rightkit-logs/tests/scrub-parity.v1.json` is the single shared JS/Rust vector file (37 entries, replacing the 7 pairs duplicated in each suite). `index.test.ts` runs it against `scrubLogText`, `crash.test.ts` runs the `windowCrash` subset against `scrubWindowCrashText`, and the Rust `scrub_parity` suite runs it against `scrub_text`. No runtime change; the file is not part of the npm package.
- Docs: the README entry table lists every export (recorder, crash capture, both scrubbers, the crash command constant and the exported types) and describes the parity vectors.

## 0.1.5 (2026-10-19)

- Repairs crash capture and log redaction handling.

## Unreleased

- Owner gap OG16-5, JavaScript side (additive, nothing installed by default). `installWindowCrashCapture({ invoke, ... })` captures `window` `error` and `unhandledrejection`, scrubs the text (known secrets via `redactValues`, bearer tokens, JWTs, emails, long hex, home paths, URL query and fragment), caps message and stack size, rate-limits (5 per minute, 25 per page by default) and sends one `{ report }` to the app-registered `rightkit_crash_window_error` command (`RIGHTKIT_CRASH_WINDOW_ERROR_COMMAND`). Send failures are swallowed. Rust half: `rightkit-crash` feature `window-errors`.
