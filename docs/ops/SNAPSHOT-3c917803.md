# Consumer snapshot 3c917803

> Historical; method not allowed under GATE-CONTRACT §1 (docs/GATE-CONTRACT.md, locked 2026-10-28): no clone, `git archive` export or snapshot outside the gate-owned directory.

Named 2026-10-12 08:05 IST by the RightKit lead. Supersedes snapshot c9f222a1. A further snapshot follows within hours (see "Known defects").

Commit: bogusyogi/rightkit main `3c917803`.
Mac path: `/Volumes/D/rightkit-publish/snapshots/consumer-3c917803/claude/rightkit` (plain `git archive`, the whole repository).
Dell: `git archive 3c917803` from the Dell clone.

Nothing here is published. Copy crates into the app's `rk-snapshot/` by relative path and keep the `# RIGHTKIT OVERRIDE` lines, each naming this commit.

## Evidence

The first main run that is green on both hosts: run 38189661157, macOS and Windows, every unit (Windows 87 of 87 against base 0f0395ee), no known reds. Second comparative reads at this commit: rightkit-secrets and rightkit-pdf pass; rightkit-fs, rightkit-process and rightkit-http each had a finding (below).

## Why you need it: the macOS database-lock regression is fixed (ledger 205)

Snapshot c9f222a1's rightkit-fs opened and closed an existing file to read its ACL; rightkit-db applied that to a database and its `-wal` and `-shm` at every open, which released SQLite's POSIX locks. Two processes on one database could then crash with SIGBUS in the WAL index, and nothing excluded a second writer. Here the ACL is read by path and nothing is opened (`crates/rightkit-fs/src/acl.rs`, `src/private.rs`; test `tests/private_keeps_locks.rs`). Every macOS consumer moves off c9f222a1.

## Version numbers still do not tell snapshots apart

Changed since c9f222a1 with NO version change: cache, crash, fs, fsindex (tests only), http, jobs, logs, media (catalogue), pdf, process, secrets, service. Everything else is identical to c9f222a1.

## What changes for a consumer

**rightkit-fs**: a refused lock file is never unlinked. `restrict_owner_only` and `private::is_owner_only` open nothing. No API change.

**rightkit-secrets**: on Windows every mutation takes the new target lock and then the lock 0.1.0 to 0.1.3 take, so an old and a new build of one app cannot both create one database key. Passed its second comparative read. The rule on real keychains is unchanged until it is published.

**rightkit-process**: `StderrTail::finish_within` no longer joins as soon as the drain body ends.

**rightkit-http**: `download_to_path` refuses a destination whose resolved parent is inside `.rk-download`; on Unix partials are `0600`, an existing regular destination keeps its mode (masked to `0777`), a fresh one is published `0600`. On Windows the path written through is the verbatim form; the report still returns the caller's path.

**rightkit-pdf**: a changed-font replacement carries the source run's full placement (text matrix times CTM) and compose refuses one that lands elsewhere (`verification_failed`); it is placed from the island, not from `availableBounds.x/y`; `sizePt` is the island's own text-space value. Island grouping measures its tolerance in device space, so on pages with a scaled CTM islands that were wrongly merged are now separate and locators taken with an older build do not match there. A replacement now sits on the true baseline and can join a same-colour neighbour on its line into one island. Passed its second comparative read; ViewRight's rendered-pixel probe: misplaced changed-font replacements 24 of 407 before, 0 of 424 after.

**rightkit-jobs**: every settle (the owner's and the runner's) holds the flush lock, the owner's insert-or-replace is one step, and the discard of a held write runs under the same lock, so an owner settle cannot lose to a concurrent flush or runner (both passes of the held-write fix are in this commit). An owner settle that arrives just after a flush stored the runner's outcome returns `illegal_transition`: read the row, do not retry. The runner's deferral (`with_settle_later`) may be switched on again for testing against this snapshot.

**rightkit-crash**: an unscrubbed process sample never sits under a name the support bundle copies; `CrashReport.native_capture_note` is a new optional field.

**rightkit-logs, rightkit-cache, rightkit-jobs, rightkit-service**: a read that is refused is an error or "unknown", never "absent" (install id, consent, cache pins, journal replay, service records).

**rightkit-media**: the catalogue approves `ffmpeg-7.1.5-export-ass-zimg-macos-arm64`; the README says to state a rate for `h264_mf` on Windows.

**rightkit-browser** (unchanged since c9f222a1, repeated because the last note missed it): on Windows a Chrome whose folder lacks the AppContainer grants is refused; use the managed Chrome for Testing.

## Known defects in this snapshot, fixed on main, in the next snapshot

- rightkit-http: a destination written as `file/` or `file/.` is normalised onto `file` and overwrites it; and on macOS the staging directory and partial inherit a parent's ACL `allow` entries. Do not pass a destination that ends in a separator or a dot.
- rightkit-fs: the macOS ACL check of a parent directory opens its argument plainly; if a caller passes a regular file where a directory is expected (`create_private_new(file.join("x"))`), that file is opened and closed, which releases locks on it.
- rightkit-process: `finish_within` can still wait on thread-local destructors of the caller's callbacks.
- rightkit-jobs: two stores or two processes replaying one journal directory are not coordinated (ledger 207); keep one `JobStore` per database per machine.
- rightkit-qa on Windows: dispose fails on a profile that contains the WinINet `Content.IE5` junction and removes the binding file first (ledger 216).
