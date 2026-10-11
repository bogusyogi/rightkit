# Changelog

## Unreleased

- Panic hook fixes (consumer-reported). A sink whose thread cannot be spawned now runs inline instead of losing the record; the re-entrancy guard is per thread, so a panic on another thread during a slow sink is recorded (the sink thread holds its own guard, so a sink panic still cannot recurse); the file record is written before the sink runs, so under `panic = "abort"` it survives a panicking sink, and `with_sink` documents unwind-only containment; sink-only mode no longer creates `crash_dir`. Additive: `CrashConfig::capture_backtrace(bool)` (default `true`, unchanged behaviour) skips the unbounded backtrace capture in the panic hook. `rightkit-logs` requirement raised to `0.2.2`. Breaking: none.
- Panic capture hardening: the re-entrancy flag is cleared by a drop guard, so a failed record no longer disables later reports; reports are written owner-only (0600 on Unix, current-user-only DACL on Windows) through `rightkit-fs` atomic writes; `set_recording_enabled` / `recording_enabled` add a runtime off switch (default on). Additive: `CrashConfig::with_sink` hands each redacted report to a host callback (run on a bounded thread, so a sink panic is contained), and `CrashConfig::write_report_file(false)` turns off the `crash_dir` file. Breaking: none.

## 0.2.1

- **`support-bundle` feature (default on).** The `zip` dependency is now optional behind `support-bundle`, which gates `build_support_bundle`, `SupportBundleOptions`, `BundleManifest` and `create_zip` (about 200 lines). Panic capture, pending-report handoff, native capture and retention are unchanged without it. Default builds are unchanged.
