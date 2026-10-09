# Changelog

## 0.4.0

- **Atomic path swap.** New `swap` module: `swap_paths(a, b)` exchanges two existing paths with macOS `renamex_np(RENAME_SWAP)` in one kernel step, so neither name is ever absent, even under SIGKILL. `is_swap_unsupported` recognises `ENOTSUP`, `EINVAL`, `ENOSYS` and `ErrorKind::Unsupported` (non-APFS volume, other OS) so callers can fall back. Other platforms return `ErrorKind::Unsupported`.
- **`rightkit-swap A B` binary** (feature `fs`). Exit 0 swapped, 1 I/O, 2 usage, 3 unsupported. Used by `right-release install-app`.

## 0.3.2

- **Durable writes fall back like libuv on Apple.** `atomic_write*`, `StateDir` publishes and directory syncs now call the new `durable_sync`: `F_FULLFSYNC`, then `F_BARRIERFSYNC`, then `fsync`. Previously `File::sync_all` (bare `F_FULLFSYNC`) failed hard with `EIO` on some external volumes under parallel writes where Node's `fsync` succeeded. Other platforms still use `sync_all`.

## 0.3.1

- **`lock` no longer depends on `nix`.** Unix record locks call `libc::fcntl(F_SETLK)` directly (same whole-file POSIX record lock, non-blocking, shared or exclusive), retrying on `EINTR`. Lock semantics are unchanged. `lock` now costs only `fs`.

## 0.3.0

- **Cargo features.** `digest` (sha2), `fs` (libc, thiserror: atomic, private, state, netfs, clone, error), `lock` (nix, on top of `fs`), and `json` (serde, on top of `lock`: identity ledger, owner records, clonefile binary). All are on by default. `rightkit-db` uses `features = ["fs"]`. Digest-only callers use `features = ["digest"]`; fs adds neither libc nor nix to their trees.
- **`digest` module (new).** `sha256_bytes`, `sha256_reader`, `sha256_file`, `sha256_file_with_len` (bytes hashed, for indexers), and `hex`. File digests stream in 64 KiB chunks. Output is lowercase hex, identical to the copies it replaces.
- **Documented primitives.** `atomic_write` and the two lock primitives, `FileLock::acquire` (exclusive) and `FileLock::acquire_shared`, are unchanged in behaviour and now documented as the shared entry points.
- Without `json`, locks behave the same but write no owner record, so `recovered_stale` is `None` and `current_owner` returns `None`.

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
