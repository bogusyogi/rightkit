# Changelog

## 0.1.3 (2026-11-16)

Byte-exact snapshots, additively: no existing function, file name or on-disk file changes for a text caller.

- New `HistoryStore::snapshot_bytes`, `record_after_bytes`, `record_after_file`, `peek_latest_bytes` / `consume_snapshot_bytes` (with `HistorySnapshotBytes`), `restore_latest_bytes` and `can_record`. A byte snapshot is the same `<ts>.snap` file `snapshot` writes (the bytes as they are), under the same slot, with the same `PATH`, `.after` and `redo` files, so a text snapshot reads back identically through the byte readers and a byte snapshot of valid UTF-8 is indistinguishable from a text one. `record_after_bytes` stores the same FNV-1a fingerprint as `record_after`, now computed over raw bytes (equal for text); `record_after_file` computes it from the file on disk in 64 KiB chunks, for a file too large to hold. `peek_latest_bytes` returns `Ok(None)` for a slot whose newest entry is a created-file marker with no snapshot under it, and an error when one lies under it (an older snapshot would be written over the created file); `peek_latest` is unchanged. `can_record` is `false` when the slot of a path is already owned by a different path (the flat slot key joins `a/b_c` and `a_b/c`), so a caller that records for a file it cannot name beforehand can fail closed instead of mixing two files' history.
- `restore_since` and `redo_since` now read and compare raw bytes, so a byte snapshot is restored exactly and a non-UTF-8 file on disk is judged by its content (it used to compare as absent); for UTF-8 content they behave as before. They also refuse, and report, a target that is a symlink or whose parent directory is a symlink leaving the workspace, instead of writing through it.
- Unchanged and not ported from the host fork: the injective slot key (it renames every slot), per-slot retention, the redo stash kept until a rewind restores something, fail-closed rewind when the post-edit record is missing, and the atomic write.

## 0.1.2
- Synced with CodeRight: snapshots are session-scoped (`HistoryStore::new(workspace, state_dir, session_id)`, `fork_from` takes `state_dir`), plus redo, restore reports (`RestoreReport`), `HISTORY_UNAVAILABLE` and `rfc3339_utc_to_ns`.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-history` (engine).
