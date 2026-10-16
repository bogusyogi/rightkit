# Changelog

## 0.3.2

- **Android log directory and logcat mirror (rightkit#204).** New `rightkit_logs::android`: `android_logs_dir(files_dir)` returns `<files_dir>/logs` from the injected app files directory (no `HOME`/`XDG`/environment lookup, no filesystem access; `None` for an empty path), matching `rightkit_android::AppPaths::logs_dir`; pass the result as `TracingFileConfig::dir`. Also `LogcatPriority` (maps to `ANDROID_LOG_*` 2 to 6) and `logcat_tag` (23-character cut, `RightKit` default). With the `tracing` feature: `LogcatLayer::new(tag, sink)`, a `tracing_subscriber` layer that mirrors each event, passed through `scrub_text`, to a `LogcatSink`, and `priority_for_level` (TRACE verbose, then DEBUG, INFO, WARN, ERROR).
- Design: no second liblog binding. `rightkit-android` already owns `__android_log_write` and the debug-only switch (`rightkit_android::logcat::write`); an Android app builds the sink as a closure over it, so logcat stays off unless `RightKit.init(debugLogging = true)`. This crate gained no dependency and no `cfg(target_os = "android")` code.
- Tests: `android::tests` and `android::layer::tests` (path injection, priority and level mapping, tag cut, sink receives tag and priority). Not compiled or run; not built for an Android target.

## 0.3.1

- **Android compile pass (rightkit#152).** No source path selects a macOS-only or Linux-only API for `target_os = "android"`: the macOS (`libc` `sysctl`, mach) and Windows (`windows-sys`) code is gated by `target_os` and Android takes the portable fallback in `system/identity.rs` and `system/metrics.rs`; there is no syslog, `os_log`, xattr, keychain or `getpwuid` use. Log directories are always passed in by the caller (the crate has no path discovery), so an Android app passes `Context.getFilesDir()`-based paths. `ureq` is built with `rustls` and no platform verifier, so uploads use the bundled web PKI roots and need no JNI.
- Added: on Android `platform_identity().memory_total_mb` is now read from `/proc/meminfo` (`MemTotal`) instead of 0; other fields stay `"unknown"` there. New private `parse_meminfo_total_mb`, tested on the host (`system::identity::tests`). Linux and other unix targets keep 0.
- Not done, by design: a logcat sink (needs `liblog`; it belongs to the `rightkit-android` bridge, rightkit#149). `system_load_1m` still reads `/proc/loadavg`, which Android may refuse an app; a refusal reads as an absent value. Not built or run for an Android target.


## 0.3.0

- Republished on the renamed dependencies: `rightkit-child-process` (was `rightkit-process`). The 0.2.2 on crates.io was packaged before the renames and still named the old crates.
- **Breaking (0.x minor):** the `process` field of the outbox and file-writer usage records is now `rightkit_child_process::ProcessUsage`.
## 0.2.2

- Added: opt-in full-path redaction. New `scrub::ScrubOptions { redact_paths: bool }` (default `false`) and `scrub::scrub_text_with(&str, &ScrubOptions)`; with `redact_paths` every absolute path token (`/…`, `~/…`, `C:\…`, `\\server\…`) becomes `[path]`, matching the CodeRight support bundle. URLs and ratios such as `1/2` are kept. `scrub_text` is unchanged and keeps the shape-preserving default. Tests: `tests/scrub_parity.rs`. Not compiled or run locally; verification is the CI run.

- Added: `scrub_text` now covers the free-text patterns the CodeRight support bundle redacts and the owner scrubber lacked (owner gap M15 O5). New: `Bearer <token>` drops the token after a whole-word `Bearer` marker (the marker stays; a marker with no token after it is kept); Telegram bot tokens (`<6+ digits>:<20+ alphanumeric, _ or ->`) and `csk-` keys are redacted as `[redacted:credential]`; `or-` keys are redacted when 20+ characters and containing a digit. Additive; signatures are unchanged. Deliberate differences from CodeRight: `scrub_text` masks only the user segment of home directories and keeps other absolute paths' shape (see `tests/scrub_parity.rs`), and short `sk-`/`csk-` words under 12 characters are kept. Tests: `tests/scrub_parity.rs` (one positive and one no-false-positive case per pattern). Not compiled or run locally; verification is the CI run.
- Fix: `identity::install_id` no longer replaces the install id because its file could not be read. Only an absent file (`NotFound`), or a file that was read and holds no valid id (including bytes that are not UTF-8), creates a new id. Any other read error (a permission error, a Windows sharing violation while another process replaces the file, an I/O error) is returned as `Err` and nothing is written. The signature is unchanged (`io::Result<String>`). Before, any read error generated a new id and overwrote the file, silently changing the install's identity.
- Fix: a consent file that cannot be read is no longer indistinguishable from "the user never chose". `ConsentStore::status` still returns `Consent::Unset` (so nothing uploads), but the reason is now kept: new `ConsentStore::last_read_error() -> Option<String>` returns the message of the latest `status` call that failed for a reason other than the file being absent (an unreadable file, or content that is not valid consent state), shared between clones and cleared by the next good read. New `ConsentStore::try_status() -> io::Result<Consent>` returns `Ok(Unset)` for an absent file, the read error for an unreadable one, and `InvalidData` for corrupt content. Additive; nothing retried or slept.
- Tests: `identity::tests` and `consent::tests` (a directory at the path stands in for a read error that is not `NotFound`). Not compiled or run locally; verification is the CI run.
- Added multi-generation rotation (owner gap O2): `JsonlStore::with_generations(n)` keeps `.1` (newest) through `.N` (oldest) predecessors; `0` behaves as `1`. The default is 1, so `JsonlStore::new` rotates exactly as before. `clear` removes every configured generation. Diagnostics collection still reads only `.1`.
- Added a plain-text rotating writer (O3): `RotatingTextWriter::new(path, max_file_bytes).with_generations(n)`, `write_line`, `status() -> StoreStatus`, `path()`. It shares the rotation code, hard cap (twice the rotation size after a failed rotation) and failure counting with `JsonlStore`.
- Added child-process stderr capture (O1): `capture_stderr(reader, Arc<RotatingTextWriter>, prefix) -> io::Result<StderrCapture>` copies a stream (for example `ChildStderr`) on a background thread, one record per line as `prefix` + `scrub_text(line)`, into the rotating writer. `StderrCapture::join()` returns the line count. Lines over 64 KiB are split; non-UTF-8 bytes are replaced; write failures are counted by the writer and never stop the capture.
- Added the opt-in `log-bridge` feature (O6; implies `tracing`, adds `tracing-log`): `init_log_bridge() -> io::Result<()>` installs `tracing_log::LogTracer` so `log::` records reach the tracing subscriber. Default features are unchanged.
- Tests: `textlog::tests` and `tests/rotation_generations.rs`. Not compiled or run locally; verification is the CI run.

- `JsonlStore`: `append_best_effort` no longer prints to stderr; failures are counted and the last message kept in `JsonlStore::status()` (`StoreStatus`). A failed rotation now still appends the event to the current file up to a hard cap of twice the rotation size (then appends are refused and counted), and the rotation failure is counted too. Function signatures are unchanged. Additive: `AsyncWriter::spawn_reporting` with `FailureReporting::Once` reports the first failure of a run once, counts the suppressed rest, and reports the recovery once (`AsyncWriter::failure_status`); `spawn` keeps the default per-failure behaviour.
- Added the optional `tracing` feature (off by default; the default build is unchanged). It adds
  daily-rolling JSONL file logs: `install_tracing` (global subscriber), `tracing_layer` (for apps
  that compose their own subscriber), `TracingFileConfig` and `TracingGuard`.
- Files are `<file_prefix>.YYYY-MM-DD.jsonl` by UTC day. Opening a day's file prunes to the newest
  `max_files` (default 3, today's included), oldest first, touching only names of that exact form.
- Writes go through a bounded, non-blocking worker. A full queue drops and counts the line; dropping
  the guard flushes every accepted line and joins the worker.
- Diagnostics: `tracing_log_sources` and `collect_tracing_diagnostics` collect the same daily files
  through the existing bounded, redacted collection policy.
- Dependencies: `tracing` and `tracing-subscriber` are optional and pulled only by the `tracing`
  feature. The worker and rolling writer are in-crate; `tracing-appender` is not used.
- `TracingFileConfig::queue_lines` (default 128_000, the `tracing-appender` buffered-lines limit; `0` behaves as `1`). The queue was a fixed 8192 lines before this release, so bursts that dropped lines at 8192 now fit. Breaking for struct literals without `..`; use `TracingFileConfig::new` or `..TracingFileConfig::new(dir, prefix)`.
- `tracing_layer_checked::<S>(config) -> io::Result<(layer, TracingGuard)>`: the fallible form of `tracing_layer`. It returns an error for an invalid prefix or filter, an unusable directory or file open, or a worker that cannot start. `install_tracing` already returned `io::Result` on the same failures; `tracing_layer` is unchanged and never fails.
- `TracingGuard::dropped_events()` is documented in the README as the running count of lines dropped because the queue was full.
- Guard shutdown is bounded (ledger #83). Dropping `TracingGuard` queues a stop request with an acknowledgement, retrying for `TracingFileConfig::shutdown_send_timeout` (default 100 ms) on a full queue, then waits `shutdown_wait` (default 1 s) for the writer to flush and acknowledge. On either timeout the guard detaches the worker and adds the lines not yet written to `dropped_events()`. Before, shutdown joined the writer without a bound, so a stalled filesystem hung app exit. Breaking for struct literals of `TracingFileConfig` without `..`, as with `queue_lines`.

## 0.2.1

- Dependencies: `chrono` removed (event timestamps are parsed by a small RFC 3339 parser) and
  `objc2-foundation` removed (macOS physical memory is read with `sysctl hw.memsize`; thermal
  state is read from the kernel's `com.apple.system.thermalpressurelevel` notify key, the source
  behind `NSProcessInfo.thermalState`, which has no sysctl key).
- Internal: crash-safe file replacement now uses `rightkit_fs::atomic_write_with` (only the
  `fs` feature: libc and thiserror, no network dependency). The `http` feature stays the only
  network dependency.
