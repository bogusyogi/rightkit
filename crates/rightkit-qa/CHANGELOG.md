# Changelog

## 0.2.15 - 2026-10-10

- `rightkit-control` is now a macOS-only target dependency with `default-features = false`; only `mac::frontmost_name` and `mac::frontmost_pid` were used, and Windows builds no longer compile it.
- Drop `libc`: orphan-sweep liveness (`storage::pid_live`) now uses `rightkit_process::AdoptedProcess::is_running` on every platform (replaces `libc::kill` on Unix and a `tasklist` spawn on Windows). A process whose state cannot be measured counts as live.
- Drop `uuid` and `time`: `util::new_id` builds the same hyphenated v4 identifier from `getrandom`, and `util::now_iso` formats RFC 3339 UTC by hand with the same shape as before. Both have tests.
- Windows foreground read (`control::frontmost`) uses `windows-sys` 0.61 (already in the lock via overlay, ort, crash, logs, hotkeys) instead of `windows` 0.62. Written and checked by inspection only: no Windows build was run for this change.
- No public API changes.
