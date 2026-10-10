# rightkit-fs

Safe filesystem state for Right Suite apps:

- Descriptor-pinned state directories (`StateDir`). A swapped or symlinked path cannot redirect a pinned directory.
- Atomic writes (`atomic_write`, `atomic_write_private`, `atomic_write_with`, `atomic_write_with_options`, `AtomicWriter`): temp file, `fsync`, then rename. `AtomicWriteOptions` adds `ReplaceMode::ReplaceFile` (Windows `ReplaceFileW`) and a `RetryPolicy` for sharing violations. A directory `fsync` failure after the replace returns `FsError::ReplacedButDirSyncFailed`. Stale temp files from crashes are swept (`sweep_stale_temps`).
- Cross-process advisory locks (`FileLock`, `LockOptions`, `with_lock`), with owner and stale-owner reporting. `LockKind` chooses POSIX record locks (`Fcntl`, the default) or `flock(2)` (`Flock`) on Unix; `probe_owner` reads the owner record without locking.
- Owner-only files and directories (`write_private_file`, `ensure_private_dir`, `create_private_new`): mode 0600 and 0700 on Unix, a restricted DACL on Windows.
- Stable file identity across renames and atomic replacement (`IdentityStore`, `StableId`; `Fingerprint` is `identity::Fingerprint`, not re-exported at the crate root, `src/lib.rs:49`).
- macOS `clonefile(2)` with `CLONE_NOFOLLOW` (`clone_file_nofollow`). It never falls back to a byte copy.
- macOS `renamex_np(RENAME_SWAP)` (`swap_paths`, `is_swap_unsupported`) and the `rightkit-swap A B` binary: atomic exchange of two paths, never absent in between.

Catalog capability: `safe-filesystem`. Published to crates.io (see [INDEX.md](../../INDEX.md)).

## When to use it

- Use it for any app state file that must not be torn by a crash, redirected by a symlink, or read by other users.
- Use it for cross-process locks on app-owned state.

Do not use it as a substitute for a database transaction. It guards single files and state directories.

## Cargo features

All features are on by default. Opt out with `default-features = false` and name only what you use.

- `digest`: `digest` module (`sha256_bytes`, `sha256_reader`, `sha256_file`, `sha256_file_with_len`, `hex`). Depends only on `sha2`.
- `fs`: atomic writes, owner-only files, `StateDir`, netfs checks, clonefile, path swap (and the `rightkit-swap` binary), and the error type. Depends on `libc` (unix) and `thiserror`.
- `lock`: cross-process locks (`FileLock`, `with_lock`). Depends on `fs` and `libc` (unix).
- `json`: identity ledger (`IdentityStore`), lock owner records, and the `rightkit-clonefile` binary. Depends on `lock`, `serde`, and `serde_json`.

Without `json`, locks write no owner file, so `recovered_stale` and `current_owner` return `None`. The lock itself behaves the same. `rightkit-db` uses `features = ["fs"]`. Digest-only callers use `features = ["digest"]`.

## Platform support

- Unix: file operations are descriptor-relative, with `O_NOFOLLOW` and `*at` calls.
- Windows: operations are handle-relative. Reparse points and symlinks are refused (`FsError` reports them).
- macOS: `clone_file_nofollow` uses `clonefile(2)`; `swap_paths` uses `renamex_np(RENAME_SWAP)` (APFS only; ENOTSUP/EINVAL elsewhere). On other platforms they return `CloneError::Unsupported` / `ErrorKind::Unsupported`.
- Network filesystems: the `netfs` module reports whether a path is on one (`is_network_filesystem`). Check it where lock semantics matter.

## Tests

- `tests/e2e.rs`: real files, real child processes, real symlink attacks and real SIGKILLs. Nothing is mocked.
- `tests/lock_spawn.rs`: locks across forking threads, and in-process `flock` semantics.
- `tests/lock_cross_process.rs`: locks held by other processes, for both kinds.
- `tests/lock_owner.rs`: owner record shapes (RFC 3339 and unix-millis) and `probe_owner`.
- `tests/clone.rs`: `clonefile` behaviour.
- `tests/swap.rs`: `RENAME_SWAP` exchange, missing-path error, and the `rightkit-swap` binary.

## Version and changes

Published version: see [INDEX.md](../../INDEX.md). Version history: [CHANGELOG.md](CHANGELOG.md).
