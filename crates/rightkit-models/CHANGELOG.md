# Changelog

## 0.2.8

- Cross-process locks now use `rightkit_fs::FileLock` (POSIX record lock on unix, `LockFileEx`
  on Windows, process-wide registry) instead of three `std::fs::File::try_lock` copies:
  the store operation guard (`lock.rs`, `<entry>.operation.lock`), the single-load model lease
  (`suite/lease.rs`) and the refs registry guard (`suite/refs.rs`). Lock files, holder files
  and the legacy `<entry>.lock` TTL marker keep their names and formats. Not replaced: the
  `<entry>.lock` marker protocol and `.holder.json`, which are data, not locks. Mixed-version
  note: on Linux, `flock` (old builds) and `fcntl` (this build) locks do not exclude each
  other; on macOS and Windows they do.
- Atomic writes: `store::write_atomic`, `suite::manifest::write_atomic` and the refs writer
  use `rightkit_fs::atomic_write` (temp file, fsync, rename, directory fsync). Temp files are
  now named `.<name>.rktmp-*`.
- Fix: on Windows a replaced file is no longer deleted before the rename. `store::write_atomic`
  (state.json, ledger writes), download `commit_part` (the repaired model file) and the resume
  metadata commit now replace the existing file in a single rename, so a failed commit keeps
  the previous file. `pub fn http_agent() -> ureq::Agent` is unchanged.
