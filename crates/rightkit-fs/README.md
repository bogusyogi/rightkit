# rightkit-fs

Safe filesystem state for Right Suite apps:

- Descriptor-pinned state directories (`StateDir`). A swapped or symlinked path cannot redirect a pinned directory.
- Atomic writes (`atomic_write`, `atomic_write_private`, `atomic_write_with`, `AtomicWriter`): temp file, `fsync`, then rename. Stale temp files from crashes are swept (`sweep_stale_temps`).
- Cross-process advisory locks (`FileLock`, `LockOptions`, `with_lock`), with owner and stale-owner reporting.
- Owner-only files and directories (`write_private_file`, `ensure_private_dir`, `create_private_new`): mode 0600 and 0700 on Unix, a restricted DACL on Windows.
- Stable file identity across renames and atomic replacement (`IdentityStore`, `StableId`, `Fingerprint`).
- macOS `clonefile(2)` with `CLONE_NOFOLLOW` (`clone_file_nofollow`). It never falls back to a byte copy.

Catalog capability: `safe-filesystem`. Published to crates.io (see [INDEX.md](../../INDEX.md)).

## When to use it

- Use it for any app state file that must not be torn by a crash, redirected by a symlink, or read by other users.
- Use it for cross-process locks on app-owned state.

Do not use it as a substitute for a database transaction. It guards single files and state directories.

## Cargo features

None. The crate has no `[features]` table. A `rightkit-clonefile` binary is included (`src/bin/rightkit-clonefile.rs`). Unix-only dependencies: `libc` and `nix` (`fs` feature).

## Usage

Taken from the imports and calls in `tests/e2e.rs`:

```rust
use rightkit_fs::{atomic_write, FileLock, LockOptions, StateDir};

let dir = StateDir::create(path)?;
atomic_write(&file_path, bytes)?;

let _lock = FileLock::acquire(&lock_path, &LockOptions::default())?;
```

`atomic_write` takes a path and bytes. `StateDir::open` and `StateDir::create` take a path, and `StateDir::read` takes a child name and a byte limit. See `src/atomic.rs`, `src/state.rs` and `src/lock.rs` for the full signatures.

## Platform support

- Unix: file operations are descriptor-relative, with `O_NOFOLLOW` and `*at` calls.
- Windows: operations are handle-relative. Reparse points and symlinks are refused (`FsError` reports them).
- macOS: `clone_file_nofollow` uses `clonefile(2)`. On other platforms it returns `CloneError::Unsupported`.
- Network filesystems: the `netfs` module reports whether a path is on one (`is_network_filesystem`). Check it where lock semantics matter.

## Tests

- `tests/e2e.rs`: real files, real child processes, real symlink attacks and real SIGKILLs. Nothing is mocked.
- `tests/lock_spawn.rs`: cross-process locks.
- `tests/clone.rs`: `clonefile` behaviour.

## Version and changes

Published version: see [INDEX.md](../../INDEX.md). Version history: [CHANGELOG.md](CHANGELOG.md).
