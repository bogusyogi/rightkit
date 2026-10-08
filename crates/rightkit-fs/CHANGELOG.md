# Changelog

## 0.2.0 — unpublished

- **File locks no longer look busy after a fork (Unix).** `FileLock` used `flock` through
  `File::try_lock`, which belongs to the open file description. A child forked by another
  thread (for example a `rightkit-process` spawn with `pre_exec`) inherits a lock descriptor
  until it execs. A failed `acquire` after the lock was taken (for example when the owner
  record cannot be written) dropped that descriptor without unlocking it, so the lock stayed
  held while the child lived and the next `try_acquire` reported `Locked`. Normal release was
  not affected: `Drop` calls an explicit unlock. Unix locks are now POSIX record locks
  (`fcntl F_SETLK`, through `nix`), which belong to the process and are not inherited. A
  process-wide registry keeps in-process exclusion (shared holders share one descriptor,
  exclusive is refused while anything is held), and every acquire and release runs under it,
  so one process never holds two descriptors on one lock file. Windows keeps `LockFileEx`
  through `File::try_lock` unchanged.
- Adds a `unix` dependency on `nix` (pinned `=0.30.1`, `fs` feature, already in the workspace).
- **Clone helper.** New `clone` module: macOS `clonefile(2)` with `CLONE_NOFOLLOW`. It never
  falls back to a byte copy. Ships the `rightkit-clonefile` binary.
- Crate version moves from 0.1.1 to 0.2.0.
