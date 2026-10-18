# Changelog

## 0.1.5 — unpublished staging

- Repairs crash capture and log redaction handling.

## Unreleased

- Owner gap OG16-5, JavaScript side (additive, nothing installed by default). `installWindowCrashCapture({ invoke, ... })` captures `window` `error` and `unhandledrejection`, scrubs the text (known secrets via `redactValues`, bearer tokens, JWTs, emails, long hex, home paths, URL query and fragment), caps message and stack size, rate-limits (5 per minute, 25 per page by default) and sends one `{ report }` to the app-registered `rightkit_crash_window_error` command (`RIGHTKIT_CRASH_WINDOW_ERROR_COMMAND`). Send failures are swallowed. Rust half: `rightkit-crash` feature `window-errors`.
