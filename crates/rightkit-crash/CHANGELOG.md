# Changelog

## Unreleased

- Panic capture hardening: the re-entrancy flag is cleared by a drop guard, so a failed record no longer disables later reports; reports are written owner-only (0600 on Unix, current-user-only DACL on Windows) through `rightkit-fs` atomic writes; `set_recording_enabled` / `recording_enabled` add a runtime off switch (default on). Additive: `CrashConfig::with_sink` hands each redacted report to a host callback (run on a bounded thread, so a sink panic is contained), and `CrashConfig::write_report_file(false)` turns off the `crash_dir` file. Breaking: none.

## 0.2.1

- **`support-bundle` feature (default on).** The `zip` dependency is now optional behind `support-bundle`, which gates `build_support_bundle`, `SupportBundleOptions`, `BundleManifest` and `create_zip` (about 200 lines). Panic capture, pending-report handoff, native capture and retention are unchanged without it. Default builds are unchanged.
