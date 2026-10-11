# Changelog

## Unreleased

- Fix: `identity::install_id` no longer replaces the install id because its file could not be read. Only an absent file (`NotFound`), or a file that was read and holds no valid id (including bytes that are not UTF-8), creates a new id. Any other read error (a permission error, a Windows sharing violation while another process replaces the file, an I/O error) is returned as `Err` and nothing is written. The signature is unchanged (`io::Result<String>`). Before, any read error generated a new id and overwrote the file, silently changing the install's identity.
- Fix: a consent file that cannot be read is no longer indistinguishable from "the user never chose". `ConsentStore::status` still returns `Consent::Unset` (so nothing uploads), but the reason is now kept: new `ConsentStore::last_read_error() -> Option<String>` returns the message of the latest `status` call that failed for a reason other than the file being absent (an unreadable file, or content that is not valid consent state), shared between clones and cleared by the next good read. New `ConsentStore::try_status() -> io::Result<Consent>` returns `Ok(Unset)` for an absent file, the read error for an unreadable one, and `InvalidData` for corrupt content. Additive; nothing retried or slept.
- Tests: `identity::tests` and `consent::tests` (a directory at the path stands in for a read error that is not `NotFound`). Not compiled or run locally; verification is the CI run.

## 0.2.2

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
