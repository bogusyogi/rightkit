# Changelog

## 0.1.1

- `atomic_write` now delegates to `rightkit_fs::atomic_write` (unique temp file, `fsync`, rename, directory `fsync`). The hand-declared `ReplaceFileW` FFI and the Windows branch are removed; the crate has no `cfg(windows)` code. Adds the `rightkit-fs` dependency (`fs` feature only).
- `atomic_write_with` keeps its signature as the failure-injection hook; its default replace step is `fs::rename`, which replaces an existing file on every platform.
