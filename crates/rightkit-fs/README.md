# rightkit-fs

Safe filesystem state for Right Suite apps:

- Descriptor-pinned state directories (`StateDir`). A swapped or symlinked path cannot redirect a pinned directory.
- Atomic writes (`atomic_write`, `atomic_write_private`, `atomic_write_with`, `atomic_write_with_options`, `AtomicWriter`): temp file, `fsync`, then rename. `AtomicWriteOptions` adds `ReplaceMode::ReplaceFile` (Windows `ReplaceFileW`) and a `RetryPolicy` for sharing violations. A failure after the content is published is a typed error with `FsError::target_replaced()` true (`ReplacedButDirSyncFailed`, `ReplacedButPostStepFailed`). Stale temp files from crashes are swept (`sweep_stale_temps`), and only complete generated temp names are ever swept. What a durable write guarantees differs by platform: see [Atomic writes](#atomic-writes).
- Cross-process advisory locks (`FileLock`, `LockOptions`, `with_lock`), with owner and stale-owner reporting. `LockKind` chooses POSIX record locks (`Fcntl`, the default) or `flock(2)` (`Flock`) on Unix; `probe_owner` reads the owner record without locking.
- Owner-only files and directories (`write_private_file`, `ensure_private_dir`, `create_private_new`): mode 0600 and 0700 on Unix, a restricted DACL on Windows.
- Stable file identity across renames and atomic replacement (`IdentityStore`, `StableId`; `Fingerprint` is `identity::Fingerprint`, not re-exported at the crate root, `src/lib.rs:49`).
- macOS `clonefile(2)` with `CLONE_NOFOLLOW` (`clone_file_nofollow`). It never falls back to a byte copy. `clone_or_link_or_copy_nofollow` is the explicit ladder for callers that can accept less: clone, then hard link, then a durable copy, reporting which one it was.
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

Without `json`, locks write no owner file, so `recovered_stale` and `current_owner` return `None`. The lock itself behaves the same. `rightkit-db` uses `features = ["fs"]`. Digest-only callers use `features = ["digest"]`. Every integration test names the features it imports (`required-features`), so `--no-default-features` and each single feature compile.
- `testing` (debug builds only): fault and event seams for consumer tests: `atomic::testing`, `state::testing`, `clone::testing`.

## State directories (`StateDir`)

`StateDir::open`/`create` use `StateOptions::default()`; `open_with`/`create_with` take explicit options. Children from `child` inherit them.

- **Durability.** `write`, `write_new`, `remove` and a creating `child` flush the file and then the directory, and a failed directory flush is an error (`ReplacedButDirSyncFailed` for writes, `DirSyncFailed` for removal and creation). `StateDir::create("new/deep")` publishes every directory it makes: on Unix it syncs the *parent* of each newly created directory before descending into it (so a later durable write cannot lose the ancestor links) and stops with `DirSyncFailed` if a sync fails; a retry that finds the directory already present does not sync again. **Windows** has no directory flush: `FlushFileBuffers` needs `GENERIC_WRITE`, and on the pinned list-access handle it fails with `ERROR_ACCESS_DENIED`. There, the file is flushed before publication, and creation (every directory `create` makes, including missing ancestors), the publishing rename and the delete are performed on `FILE_WRITE_THROUGH` handles. That asks the filesystem to commit each operation before the call returns; it is the strongest equivalent available, Microsoft documents no guarantee for directory metadata, and no power-loss test has been run.
- **Sync level.** On Apple targets the flush is `F_FULLFSYNC`, falling back to `F_BARRIERFSYNC` then `fsync` on volumes that refuse it. `durable_sync` keeps that silent fallback. `durable_sync_with(file, policy)` returns the `SyncLevel` reached (`Fsync < Barrier < Full`), and `SyncPolicy::Strict` (also `StateOptions::sync`) returns the `F_FULLFSYNC` error instead of falling back. `write_reporting` and `write_new_reporting` return the weakest level the file and directory flushes reached. Non-Apple targets always report `Full`, their strongest flush.
- **Names.** Reads, stats, removals and listings accept any name up to the platform limit (255 bytes on unix, 255 UTF-16 units on Windows), so an existing record longer than the write cap never fails a listing. Writes and `child` are held to `StateOptions::max_name_len` (default 200 bytes on unix, 255 units on Windows; clamped to the platform limit). A temp name carries at most 64 bytes of the real name, so a 255-byte name can be written. A write target shaped like a generated temp name (`.<name>.rktmp-<pid>-<n>-<attempt>`) is refused with `BadName`, because `sweep_temps` deletes such names; a name that merely contains `.rktmp-` is an ordinary record and is never swept.
- **Reads.** `read(name, max_bytes)` refuses a file over `max_bytes`; `u64::MAX` means no limit.
- **Listing.** Each listing opens its own directory description (`openat(dirfd, ".")` on Unix; Windows serialises enumeration), so concurrent listings from several threads each see every entry.
- **Listing.** `list` returns names. `list_entries` returns `DirEntry { name, kind }` from the directory record (`d_type` on unix, `FILE_ID_BOTH_DIR_INFO` attributes on Windows), with no stat per entry; a unix filesystem that reports no type costs one descriptor-relative `fstatat` for that entry. Windows reports any reparse point as `Symlink`.
- **`allow_rename_while_open` (Windows).** Default `false`: the pinned directory is opened without `FILE_SHARE_DELETE`, so no process can rename or delete it (or an ancestor) while the `StateDir` is open, and `path()` stays true. The 0.1.1 change that introduced handle-relative operations chose this on purpose (module docs: "without delete sharing"; test `pinned_directory_itself_cannot_be_renamed_or_removed`) and records no further reason. The guarantee it gives: the pinned path cannot be renamed away and replaced (for example by a junction) underneath users of `path()`. `true` shares delete: the directory may be renamed or deleted under the open `StateDir`. Operations stay handle-relative and keep acting on the original directory, but `path()` then names whatever is there now, so use no path-based call on it. Unix descriptors always behave this way.
- **`privacy` (Windows ACL).** Default `Privacy::OwnerOnly`: directories and files the `StateDir` creates, including missing ancestors, get a protected owner-only DACL. If the `StateDir` is the first creator of an application data root, that root is protected and its inheritable ACE propagates to everything created under it later (other accounts, services and backup agents lose access). Either create the data root first, or pass `Privacy::OwnerOnlyUnixInheritWindows` (or `Inherit`): no ACL step, so new objects carry the parent's inherited ACL, as for atomic writes. Unix stays `0700`/`0600` regardless.

## Atomic writes

A write is two phases. Until the replace call succeeds nothing is published: a failure leaves the destination as it was and removes the temp file. After it, the content is in place; the required directory sync still runs, and a later failure is a typed error with `target_replaced()` true: `ReplacedButDirSyncFailed`, or `ReplacedButPostStepFailed { step }` (`TempCleanup`: a leftover second name after the `link` fallback; `OwnerOnlyAcl`: the Windows owner-only check after `ReplaceFileW`). The most serious is reported: ACL, then directory sync, then cleanup. `FsError` is `#[non_exhaustive]`.

What a durable write (`durable: true`, the default) guarantees when it returns `Ok`, by platform and mode. `publication_durability(&options, destination_exists)` returns the same answer as a value (`PublicationDurability`):

| Platform and mode | Guarantee |
|---|---|
| Unix, any mode | The temp file is flushed (`F_FULLFSYNC` first on Apple), the replace runs, the parent directory is flushed: `DirectoryFlushed`. |
| Windows `MoveWriteThrough`, `NoReplace`, and `ReplaceFile`'s first creation | The temp file is flushed (`FlushFileBuffers`), then `MoveFileExW` with `MOVEFILE_WRITE_THROUGH`, which Microsoft documents as not returning until the move is on disk: `WriteThroughMove`. No power-loss test has been run. |
| Windows `Rename` (the default mode) | The temp file is flushed, then `std::fs::rename`. The rename is not write-through and there is no directory flush, so after a power loss the previous version can reappear (the file is never torn): `FileFlushedOnly`. |
| Windows `ReplaceFile` over an existing file | The temp file is flushed, then `ReplaceFileW` with flag word 0. Its `REPLACEFILE_WRITE_THROUGH` flag is documented as unsupported and is no longer passed, so the swap is not flushed: `FileFlushedOnly`. Use `MoveWriteThrough` where the replacement must be on stable storage. |
| `durable: false`, any platform | Nothing is flushed: `None`. |

**Private writes on Windows.** `Privacy::OwnerOnly` (and `private: true`) creates the temp file with its owner-only security descriptor passed to `CreateFileW`, so it never exists with the parent's inherited ACL. `ReplaceFileW` keeps the destination's ACL, so a private `ReplaceFile` write first narrows an existing destination and fails closed: if that cannot be done nothing is published and the temp file is removed. Unix creates the temp with mode `0600`.

**Names.** Public writes (and `rename_exclusive`) refuse a target whose name is a complete generated temp name; `sweep_stale_temps` deletes only such names.

## Clone, link, copy

`clone_or_link_or_copy_nofollow(src, dst)` (and `_with(src, dst, &CloneOptions)`) creates `dst` from the regular file `src` by the first step that works and returns which one: `CloneOutcome::Cloned` (macOS `clonefile`, Linux `FICLONE` where the filesystem has reflinks; Windows has no clone step), `HardLinked` (same volume), or `Copied` (a durable `NoReplace` atomic write; Unix permission bits are copied). A link at `src` or `dst` is refused (`FsError::Symlink`), never followed; an existing `dst` is `FsError::AlreadyExists` and never overwritten. **A hard link shares content with the source**: an in-place write through either name changes both, and deleting the destination removes only a name, so a caller that tracks copies should treat `HardLinked` ones as one item. Set `CloneOptions::allow_hard_link = false` (or `.without_hard_link()`) when the copy will be modified in place.

## Locks

- `LockKind::Fcntl` (default) is a POSIX record lock; `LockKind::Flock` is `flock(2)`. Both are public (`FileLock::acquire_kind`, `acquire_shared_kind`, `try_lock_with`) and tested (`tests/lock_spawn.rs`, `tests/lock_cross_process.rs`). Windows uses `LockFileEx` for both. **Fcntl and flock do not promise to exclude each other.** A probe on macOS (Darwin 27) saw each refuse the other across processes, but that is an observation, not a guarantee, and other kernels differ (Linux treats them as independent locks). Give one lock file one kind for every user.
- **One file is one lock, whatever it is called.** The Unix `Fcntl` registry is keyed by the opened file's `(dev, ino)`, not by a path: a hard link, a case alias on a case-insensitive volume and a symlinked directory all reach the same entry. POSIX record locks belong to the process and the file, so a second descriptor would neither conflict with the first nor be safe to close; the crate keeps any alias descriptor open until the entry's last holder releases, so a refused or released alias never drops a lock someone still holds. (`tests/lock_alias.rs`.)
- The lock file is opened without following a link at its name: `O_NOFOLLOW` on unix, `FILE_FLAG_OPEN_REPARSE_POINT` plus a reparse-attribute check on Windows. A link there is `FsError::Symlink`. Only the final component is checked.
- **Migrating from a create-new marker lock.** Not implemented as an API; the guidance is below. A consumer whose old versions take a lock by creating a marker file (`O_EXCL`/`create_new`, removed on release) cannot simply point `FileLock` at the same path during an update overlap. The new lock never deletes its file and holds no marker semantics, so the old version's staleness rule would see a permanent file and could delete it, and the old version would not see the new lock as held. To migrate:
  1. Give `FileLock` its own path (for example `<name>.flock`), never the marker path.
  2. For as long as old versions can run, take the `FileLock` first (it orders new versions among themselves), then create the old marker with the old format (`create_private_new`), treating `AlreadyExists` by the old version's own staleness rule, and remove the marker before releasing the `FileLock`. Old versions then see the marker; new versions serialize on the lock.
  3. A crash between the two steps leaves a marker only the old staleness rule can clear, exactly as before; the new lock needs no cleanup.
  4. Once old versions are gone, delete the marker code and the marker path.

  The marker format and staleness rule belong to the consumer, so a generic mode would need both as parameters and would still leave orphaned markers to the consumer's policy; that is why this is guidance, not a mode.

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
- `tests/state_durability.rs`: `StateDir` directory-sync failures, name caps, sync policy, listing kinds, options, unlimited reads, concurrent listings.
- `tests/state_ancestors.rs`: `StateDir::create` syncs each new directory's parent before descending (event order), and a failed sync stops the walk.
- `tests/atomic_publish.rs`: order of flush, replace and directory sync; owner-only before publication; Windows fail-closed hardening; typed post-publication errors.
- `tests/atomic_namespace.rs`: the temp namespace is reserved; a sweep never deletes a committed record.
- `tests/lock_alias.rs`: hard links, case aliases and symlinked directories are one lock.
- `tests/lock_reparse.rs`: a link at the lock file's name is refused.
- `tests/clone_fallback.rs`: clone, link and copy outcomes through the step seam; refusals.
- `tests/clone.rs`: `clonefile` behaviour.
- `tests/swap.rs`: `RENAME_SWAP` exchange, missing-path error, and the `rightkit-swap` binary.

## Version and changes

Published version: see [INDEX.md](../../INDEX.md). Version history: [CHANGELOG.md](CHANGELOG.md).
