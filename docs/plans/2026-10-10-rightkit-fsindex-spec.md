# rightkit-fsindex — specification (v3)

Status: proposed 2026-10-10, revision 3 after two Astra adversarial passes (`2026-10-10-rightkit-fsindex-spec-astra-review.md`). v2 separated shipped behaviour from guarantees; v3 closes the second pass: lock placement, privacy model, cloud no-recall, bounded replay and retention, policy snapshots, a frozen schema and version matrix. Decisions marked **[Adrian]** are open. §15 maps both passes.

## 1. What it is

One live file index for the Right Suite: a crawl-once, keep-current catalogue of entries under explicit roots, with fuzzy name search, folder-size rollups that understand hard links and clones, and a change feed, persisted in memory-mapped column files that reopen without a crawl when the store validates.

| Profile | Roots | Consumers | What matters |
|---|---|---|---|
| **volume** | whole disks / home | Pulse | crawl speed, accounting with stated error bounds, memory per entry, unprivileged upkeep, privacy of the catalogue |
| **workspace** | repository roots | Blueprint, Ledger, CodeRight tools and `@` picker (via adapters) | ignore-policy fidelity, complete and generation-bound listings, a sound change feed |

Non-goals (owned elsewhere): query syntax and FTS (`rightkit-search`), subscriber/debounce watching API (`rightkit-watch`), content ranking and embeddings (Ledger FTS5, Blueprint BM25, `rightkit-embed`), graphs and parsing (Blueprint), reclaim advice, history and presentation (Pulse). `rightkit-fs::identity` is widened together with fsindex (§6.1). The `content` feature stays opt-in and outside every consumer plan.

## 2. Consumers and what each keeps

The index **feeds** enumeration and change notification. No consumer deletes its own logic until its parity gate (§12) passes.

| Consumer | Today (verified against source) | Takes from the index | Keeps |
|---|---|---|---|
| Pulse hub, Mac | `disk_index.rs` on fsindex 0.1.0, indexing mount roots and relying on fsindex's protected-folder exclusions (`disk_index.rs:198`); `core/src/scan.rs` (identity re-checks, placeholders, reclaim upper bound withheld when incomplete, uncertainty reasons, cancellation) and `folder_growth.rs` (gap accounting); `hub/src-tauri/src/scanner.rs` (top-N per folder, name index, `incomplete`/`needs_access`/`unread`) | enumeration, sizes, change feed, access and coverage report | reclaim bounds, uncertainty reasons, history, cancellation UX, presentation |
| Pulse hub, Windows | `disk_index.rs` stubs; `watch_windows.rs`; 7-minute per-file walk | 0.2.0 backend | same |
| CodeRight `@` picker | re-walks per keystroke, 10k cap, `to_lowercase` match over workspace-relative paths, files only, highlight indices | 0.3.0 `search` with `SearchScope::RelativePath` and match indices | ranking, UI |
| CodeRight tools `list_files`/`search` | `ignore::WalkBuilder` with `git_global(true)`, noise-dir filter, globset; regex paging with deadline | 0.3.0 `list()` through the `coderight` adapter | regex search, paging, budgets |
| CodeRight repo map | consumes caller-supplied content; discovers nothing | nothing | — |
| Blueprint | `read_dir` walk that follows links confined to the root and records dangling/escaping dispositions; watch snapshot skips links; root `.gitignore` + canonical ignored dirs; barrier returns `GapBlocked|CaughtUp|Timeout` with source/applied clocks | 0.3.0 `list()` with link targets and dispositions through the `blueprint` adapter; feed cursor + fallible barrier | graph, parsing, freshness receipts, its own barrier semantics |
| Ledger | per-directory `.gitignore` only, mandatory exclusions, policy-file byte and symlink checks, records missing policies and revalidates, `*.md` ASCII-case-insensitive, re-verifies absence before tombstoning | 0.3.0 `list()` through the `ledger` adapter, view-bound with coverage | FTS5, policy validation, final absence revalidation |
| `rightkit-search::filename` | kind/extension/size filters, offset/limit; no users | retires only after 0.3.0 proves the same filters and paging | — |
| xtool | ripgrep + `fs.readdir` (Node) | deferred | — |
| ViewRight, MailRight, CutRight, GenRight, ScrapeRight, WorkRight | per-document watch or small listings | not consumers | — |

## 3. Architecture

```
rightkit-fsindex
├── core      Index API, worker, feed (index_id + epoch + seq, per-subscriber queues), rollups, search
├── store     RKFSIX03 mmap columns, names arena, identity map, header, lock, validation, durable swap
├── path      lossless native names; canonical (lookup), display, search forms
├── backend   trait Backend { stat, crawl, list_dir, watch, checkpoint, access }
│   ├── macos   getattrlistbulk + FSEvents (shipped; access tri-state in 0.2.0)
│   ├── windows FileIdExtdDirectoryInfo + ReadDirectoryChangesW + probed USN replay (0.2.0)
│   └── linux   later
├── profile   volume | workspace: PolicyView adapters, list(), link dispositions (0.3.0)
└── content   opt-in trigram postings (unchanged; never reads recall-attributed entries)
```

Invariants: a listing's parent precedes its children; links, junctions, mount points and placeholders are recorded with a disposition and never followed or hydrated by the index; mounts and excluded folders are not descended; hard links count once per identity; the crawl is unprivileged by default and reports coverage instead of failing.

## 4. API by version

**0.1.1 (shipped, exact):** `Index::open` / `open_with_backend` (block until first crawl or valid reopen), `status()`, `search(q) -> Result<Vec<Hit>>` (limit 100, 0→1, basename scope), `search_with(q, &SearchOptions{limit, scope})`, `folder_size`, `children_sorted_by` (`SortKey::Name` sorts allocation then name), `subscribe()` (bounded 16), `flush()` (no result; silent after 30 s), `snapshot()`, `trim()`, `content_search` (feature). `Item{kind, is_secondary_link, excluded, …}` (no `ext`). Config defaults per `config.rs`. `Error{Unsupported, Io, State, ContentDisabled, InvalidQuery}`.

**0.2.0 additions (all additive):** `Config{ discover_protected: bool = true, include_other_users: bool = false, roots must be explicit }`; `Status{ access: AccessReport, catching_up: bool, rebuilt_from: Option<RebuildReason>, write_failed: bool }`; `Item{ disposition: LinkDisposition{ kind, target: Option<PathBuf>, within_root: bool, dangling: bool }, lossy_name: bool }`; `SizeInfo{ unique_is_estimate: bool, files_linked_elsewhere: u32 }`; `open_async(cfg) -> (Handle, CancelToken)` with `Status.catching_up` observable; `Error::State(Busy|Corrupt|FormatUpgrade)`.

**0.3.0 additions (all additive):** `Config.profile: Volume | Workspace`; `view(adapter) -> PolicyView`; `list(&PolicyView, &ListQuery, &CancelToken) -> Result<Listing>`; `search_with` gains `SearchScope::RelativePath` and an optional `&PolicyView`; feed: `Cursor{index_id, epoch, seq}`, `subscribe_from(&Cursor, Option<&PolicyView>) -> Result<(Receiver<ChangeBatch>, Replay)>`, `since(&Cursor) -> Result<Replay>`, `barrier(timeout) -> Result<Cursor, BarrierError>`; `ChangeBatch` gains `cursor`, `prev_cursor`, `PolicyChanged`.

## 5. Store

**Format `RKFSIX03` (0.2.0; frozen for 0.2 and 0.3).** Per entry: `c-name c-parent c-first c-next c-prev c-dir` u32; `c-meta c-flags` u8; `c-nlink` u16; `c-attrs` u32 (Windows FILE_ATTRIBUTE_*, macOS flags); `c-reparse` u32; `c-vol` u64; `c-id-lo c-id-hi` u64; `c-mtime c-ctime` u64 (nanoseconds since epoch; `ctime` = ChangeTime on Windows, `st_ctime` on macOS); `c-logical c-alloc c-private c-clone` u64 = 108 B. Per directory 36 B. Names arena as today (lossless native bytes, §6.1). Header gains `index_id: u128`, `epoch: u64`, `seq: u64`, `volumes[]: {serial u64, incarnation u128 (volume GUID path hash + creation time), roots}`, `checkpoints[]: {serial, journal_id u64, applied_usn i64}`, `failed: u8`. An `RKFSIX02` store is never read; 0.2.0 rebuilds by crawl once (`rebuilt_from: FormatUpgrade`, new epoch, `index_id` preserved from a sidecar `index/id` file when present).

**Ownership.** Lock file at `state_dir/fsindex.lock` (outside the swapped `index*` directories), exclusive advisory lock acquired before recovery, creation or migration and held for the `Index` lifetime; a second opener gets `Error::State(Busy)`.

**Validation before trust.** On reopen: column sizes against header counts; every parent/first/next/prev index in range; parent chains acyclic (visited bitmap, depth ≤ 4096); name offsets inside segments; identity map rebuilt from columns. Any failure ⇒ rebuild (`rebuilt_from: Corruption`).

**Write failure.** `mark_dirty` msync and `commit` errors are not ignored: the header `failed` flag is set (msync'd), `Status.write_failed` is true, queries continue, the feed emits `Rescanned(root)` once, and the next open rebuilds.

**Durable swap.** rename `index`→`index.old`; rename `index.new`→`index`; fsync parent; remove `index.old`. `recover_swap` completes each interrupted step; the lock is unaffected because it lives outside.

**Limits stated:** reopen rebuilds the free list by scanning every slot; compaction needs > 50 000 slots, > 25 % free, 600 s since last; `add_root` errors at 64 roots. The ≈114 B/entry figure is synthetic; §12 measures real volumes.

## 6. Identity, paths, backends

### 6.1 Identity and paths (0.2.0)

- `FileId{ volume: u64, id: u128 }`; macOS: `st_dev`, inode zero-extended. Identity reuse (NTFS MFT record reuse, inode reuse) is guarded by the record's creation time: the identity map key is `(volume, id)` and the stored entry carries `c-ctime`/creation; a new occurrence with the same `(volume, id)` but a different creation time is a **replacement** (Removed+Created), not a link. `rightkit-fs::identity::FileKey` is widened to the same shape **in the same 0.2.0 release**.
- Names are stored lossless: macOS UTF-8 as returned; Windows UTF-16 as WTF-8 internally. Three forms: **canonical** (lookup; Windows: folded per the directory's case-sensitivity flag, `FILE_CASE_SENSITIVE_INFO`; macOS: per-volume case sensitivity and APFS normalization-insensitivity ⇒ NFC + simple fold), **display** (as stored; unpaired surrogates rendered lossy with `Item.lossy_name = true`), **search** (Unicode simple case folding, NFC; names with unpaired surrogates never match a query). Components split on the platform separator; verbatim `\\?\` and UNC roots are kept as given. Aliases (8.3 short names, macOS firmlinks) are not resolved; a path that resolves to the same identity through an alias is the same entry only if the backend reports the same canonical path.

### 6.2 Windows (0.2.0)

| Topic | Contract |
|---|---|
| Platforms | Windows 10 1809+; NTFS and ReFS full; exFAT/FAT32/SMB with `access.identity = Unstable` (no dedup, no USN). Enumeration class: `FileIdExtdDirectoryInfo` → `FileIdBothDirectoryInfo` (64-bit id, tag read per reparse-attributed entry) → `FileFullDirectoryInfo` (`identity = Missing`, sizes still correct). A class change restarts that directory's enumeration. |
| Crawl | one handle per directory, `BACKUP_SEMANTICS|OPEN_REPARSE_POINT`, 64 KiB pages, no per-file opens, unelevated. |
| Hard links | identity map over **all** files: open-addressing table keyed by a 64-bit hash of `(volume, id)` storing `hash:u64 + slot:u32` = 12 B per entry at ≤ 0.7 load ⇒ ≈17 B per entry resident, plus one table during rehash (peak 2×). Rebuilt from columns on reopen; never persisted. First occurrence owns; later occurrences are secondary links; ownership hands over on removal; replacement per §6.1. |
| Cloud files and placeholders | **Directories can carry recall attributes.** Any entry (file or directory) with `RECALL_ON_DATA_ACCESS`, `RECALL_ON_OPEN` or `OFFLINE` is recorded as `Placeholder` with the sizes from its parent's directory record and is **not opened or enumerated**; its subtree is reported in `Coverage.unenumerated` and the root's `AccessReport.placeholders = Skipped(n)`. A later, optional provider-aware traversal (`CfGetPlaceholderStateFromFindData`, only for directories whose state is `IN_SYNC` without recall) may descend; absent that, coverage stays incomplete rather than risking hydration. The `content` feature never opens recall-attributed entries. |
| Sizes | `logical` = default-stream EOF; `allocated` = default-stream AllocationSize; alternate streams, compressed/sparse physical use, resident MFT data and ReFS block clones are not measured; `allocated_unique` = `allocated` with `unique_is_estimate = true`. |
| Upkeep | `ReadDirectoryChangesW` per root, 64 KiB buffer, re-armed immediately after each completion. Overflow = `ERROR_NOTIFY_ENUM_DIR` **or** a successful completion with zero bytes ⇒ `Rescanned(root)`; notifications during a rescan are queued (bounded 4096; beyond ⇒ another root rescan) and applied after. Pending subtree rescans are bounded at 64; beyond ⇒ one root rescan. Three overflows within 60 s ⇒ the root switches to periodic rescan (5 min) and `AccessReport.watch = Degraded`; watch failure or root removal ⇒ `Lost`. |
| USN resume | Probe at open: `FSCTL_QUERY_USN_JOURNAL`, then `FSCTL_READ_USN_JOURNAL` with `BytesToWaitFor = 0` into a 4 KiB buffer (success returns the next USN even when no records); both must succeed unelevated or replay is `Unavailable` for that volume. Checkpoint `{journal_id, applied_usn}` is written **atomically with each commit** and never ahead of applied changes. Reopen: journal id mismatch, `applied_usn` below the journal's lowest valid USN, journal truncation records, or any read failure ⇒ recrawl that volume's roots. Replay frontier = `NextUsn` captured at open; records up to the frontier are replayed, the watch is armed before replay starts and its notifications are buffered (bounded as above) and applied after; `open()` blocks until replay reaches the frontier; `open_async` exposes `catching_up` and a `CancelToken` (cancel ⇒ the store stays clean at the last commit; next open resumes from the checkpoint). `watch = false` reopen is accepted only when the checkpoint is within journal range, else recrawl. |
| Elevated path | feature `windows-mft`, off: `FSCTL_ENUM_USN_DATA` gives names/ids, no sizes; offered only if measured to shorten the crawl. |
| Access report | `AccessReport{ denied: Vec<PathBuf>, identity: Full|Unstable|Missing per root, watch: Live|Degraded|Lost, usn: Replay|Unavailable, placeholders: Skipped(n), elevated: bool }`; `full_disk_access` stays as a derived macOS field. |

### 6.3 macOS (corrections in 0.2.0)

`has_full_disk_access` returns `Unknown` when the probe paths are absent or `HOME` is unset; never assumed true. Rollup semantics per §8.

### 6.4 Linux — later.

## 7. Change feed (0.3.0)

Today (0.1.1): `seq` is a process-global static reset on restart; `missed_batches` surfaces only on a later successful send; no per-path coalescing; new-directory contents and removed subtrees emit no per-entry events; same-size same-second edits are invisible; `flush()` is silent after 30 s.

Contract:

- `Cursor{ index_id, epoch, seq }`; `index_id` persisted; `epoch` increments on crawl, rebuild or format upgrade; `seq` monotonic within an epoch and persisted at every commit (after restart with a successful reopen the epoch is unchanged and `seq` continues; otherwise a new epoch).
- **Per-subscriber queues.** Each subscriber has its own bounded queue (16); a drop is recorded per subscriber and delivered as `missed_batches` in that subscriber's next batch, and counted in `Status.feed_gaps`.
- **Handshake.** `subscribe_from(&cursor)` runs under the feed lock: it returns the retained `Replay` from the cursor and a receiver that starts at the first batch after the replay, so nothing falls between. A cursor older than retention ⇒ `Replay::NeedsRelist`.
- **Retention bound.** Retained batches are evicted oldest-first when **any** limit is exceeded: 4096 batches, 64 MiB of batch bytes, or 15 minutes of age. `since` on an evicted cursor returns `NeedsRelist`.
- **Coalescing algebra** within `change_coalesce` per path: Created+Modified → Created; Created+Removed → nothing; Modified+Removed → Removed; Removed+Created → `Replaced` (delivered as Removed then Created in one batch; identities may differ). New directory ⇒ `Created(dir)` + `Rescanned(dir)`; removed directory ⇒ `Removed(dir)` + `Rescanned(parent)`. `Rescanned` is the only "delta not enumerated" signal and is never silent.
- **Modification detection:** `c-mtime` and `c-ctime` in nanoseconds plus size, allocation and identity; same-size same-second edits are caught by change time. Content hashing stays with consumers.
- **Barrier.** `barrier(timeout) -> Result<Cursor, BarrierError::{Timeout(cursor_so_far), Gap(cursor)}>` applies everything the backend had delivered before the call and returns the applied cursor. Blueprint keeps its own clocks and outcomes on top.
- `PolicyChanged{ adapter, digest }` is emitted when any policy file an active view read changes (§9.1).

## 8. Search and rollups (as implemented, plus 0.3.0 search forms)

Query: tokens ANDed; fuzzy subsequence over ASCII-folded bytes today, over the §6.1 search form in 0.3.0; `'x` substring, `^x` prefix, `x$` suffix, `!x` exclude; tokens > 128 bytes error; empty or exclusion-only queries error; one typo for tokens ≥ 5 bytes, not at the first character, cost 60, digits never typos. `SearchScope::RelativePath` (0.3.0) matches the workspace-relative path and returns match indices.

Rollups: hard links have one global owner; the folder holding a secondary link gets neither bytes nor count, reported via `files_linked_elsewhere`. Clone groups charge every containing folder the group-wide largest shared part (a macOS estimate; labelled `unique_is_estimate = true` when any clone group spans folders). `allocated_unique` is global attribution, not folder-local uniqueness, and is not additive across siblings. **[Adrian]** keep global attribution or move to per-folder distinct accounting (identity set per rollup update).

## 9. Workspace profile (0.3.0)

### 9.1 Policy views

`IgnoreAdapter` is a trait with three built-in versioned adapters (`coderight`: global git ignores + noise directories; `blueprint`: root `.gitignore` + canonical ignored dirs; `ledger`: per-directory `.gitignore` only, mandatory exclusions, 64 KiB/symlink checks, missing-policy records). `view(adapter) -> PolicyView` reads every policy file the adapter consults (including global ignore files outside the root) and compiles an immutable snapshot with a `digest`. `list`, `search_with`, `subscribe_from` and `since` take a `&PolicyView`; page cursors embed the view id; using a stale view returns `Error::ViewStale`. Any create/modify/delete of a consulted policy file, or appearance of a new one the adapter would consult, emits `PolicyChanged` and marks dependent views stale. Malformed policy files raise the adapter's own error (Ledger's byte and symlink rules are reproduced). Hard exclusions (`.git` directories and gitfiles, nested repositories, worktrees) are adapter-defined.

### 9.2 Links

`LinkDisposition{ kind: Symlink|Junction|MountPoint|Placeholder, target: Option<PathBuf>, within_root: bool, dangling: bool }` on every entry (0.2.0). The crawler never follows links; Blueprint resolves confined links itself from `target`; its graph-scan/watch-snapshot disagreement remains Blueprint's.

### 9.3 Listings

`list(&view, &ListQuery{ under, kinds, glob, case: CaseRule, page }, &cancel) -> Listing{ generation: Cursor, items, next, coverage: Coverage{ complete, unreadable, unenumerated, truncated, policy_digest } }`. `generation` is the feed cursor at listing start; a page cursor binds `(view id, generation, last canonical key)`; pages across a generation change return `Error::ViewStale` so the consumer re-lists or replays from `generation`. Stable canonical order; cancellation observed between pages and within a page every 4096 items. `*.md` honours `CaseRule::AsciiInsensitive`.

## 10. Privacy and permissions

- Store directories and files: owner-only (`0o700`/`0o600`; owner-only DACL on Windows); the state directory is excluded from its own index.
- **Roots are explicit** (`Config.roots`); nothing is discovered as a root. `discover_protected` (default true) keeps today's `home_dirs` behaviour of locating **protected-folder exclusions** (Library/Mail, Messages, etc.) for every home on the volume, so Pulse's mount-root indexing keeps its exclusions. `include_other_users` (default false) excludes other users' home directories found inside a volume root and lists them in `Coverage.unenumerated`.
- Access state is tri-state (`Granted|Denied|Unknown`); `allow_consent_prompts` means "may trigger TCC prompts by attempting access".
- **Subtree revocation.** Any directory under a root that becomes unreadable (EACCES/EPERM, TCC denial, `ERROR_ACCESS_DENIED`) is purged at the next commit: its slots freed, retained feed batches referencing its paths evicted, `index.old` removed, and a compaction forced within 24 h so the names arena no longer holds the names; `Rescanned(parent)` is emitted. Purge covers every retained representation; there is no retain option.
- Diagnostics redact names under denied or protected directories beyond counts.
- One store per OS user; removable volumes carry `volume.incarnation`; a changed incarnation is a new epoch for its roots.

## 11. Versions

| Version | Format | Content | Gate |
|---|---|---|---|
| 0.1.1 | RKFSIX02 | shipped 2026-10-10 | — |
| 0.2.0 | RKFSIX03 (frozen) | §5 hardening, §6.1 identity/paths (+ `rightkit-fs::FileKey`), §6.2 Windows, §6.3 macOS tri-state, §10 privacy, `open_async`, honest rollup fields | §12 platform + Pulse parity |
| 0.3.0 | RKFSIX03 | §7 feed, §9 views/listings/dispositions in list, `SearchScope::RelativePath` | consumer parity gates |
| later | — | Linux; ReFS clone probe; `windows-mft` if measured | — |

Interrupted upgrade: the rebuild writes `index.new`; the old store is untouched until the durable swap; a crash before swap leaves 0.1.1 data (unreadable to 0.2.0 ⇒ rebuild again). Cursor identity: `index_id` preserved, epoch bumped.

## 12. Qualification (release gates recorded under `release-evidence/`)

- Platform matrix: macOS APFS (clones, hard links, packages, access granted/denied/unknown); Windows NTFS and ReFS (hard links, junctions, symlinks, long paths, compressed and sparse, directory **and** file placeholders from a real OneDrive test account on the Dell, non-admin); exFAT and SMB (`Unstable`); removable volume replaced between sessions.
- Upkeep: RDCW `ERROR_NOTIFY_ENUM_DIR` and zero-byte completion; repeated overflow ⇒ `Degraded`; USN journal-id mismatch, out-of-range USN, truncation; replay cancellation mid-way then resume; same-size same-second edit; concurrent opener refused; corruption injected into each column class ⇒ rebuild; interrupted format upgrade at each swap step.
- Performance, measured on the Dell home folder on `C:` (≈2M entries) and on the Mac: cold crawl, warm reopen, catch-up, query p50/p99, update latency, **peak** working set and private bytes (identity map included), crate dependency count and build size. The first measurement sets the baseline; each later release must stay within ±10 % or document why.
- Consumer parity gates (tests in each consumer repo, run by its chat): Pulse — equal folder sizes, uncertainty reasons and reclaim bounds versus its scanner on a fixture volume, cancellation ≤ 200 ms using `CancelToken`; CodeRight picker — equal result sets and highlights versus the walk; Blueprint — equal file sets, dispositions and barrier outcomes on its fixtures; Ledger — equal markdown sets under its policy fixtures including malformed-policy rejection and missing-policy records.
- Tests are counted per platform configuration (today 17 macOS integration + 3 content + 1 burst + 2 unsupported + 3 query); Windows gets its own integration suite covering every scenario with a platform equivalent plus the items above.

## 13. Open decisions

1. **[Adrian]** Go on 0.2.0; needs the Dell (build, tests, OneDrive fixture, measurement).
2. **[Adrian]** Rollup accounting model (§8).
3. **[Adrian]** Workspace profile in fsindex (0.3.0) versus per-consumer walkers; recommendation unchanged.
4. `rightkit-search::filename` retires after 0.3.0 proves its filters and paging.

## 14. Internal consistency notes

`RKFSIX03` is defined once (§5) and frozen for 0.2 and 0.3; nanosecond times and `c-ctime` are in it from 0.2.0. `FileKey` alignment is 0.2.0 everywhere. §4 lists every public addition per version. `open_async`/`CancelToken` are 0.2.0 so Pulse's cancellation gate can be met before the Windows backend ships.

## 15. Review trail

Pass 1 (23 findings) → v2 §14 mapping. Pass 2: 7 resolved, 16 partly, 9 new P1/P2 → v3: lock outside swapped dirs and acquired first (§5); `failed` flag persisted (§5); cycle check (§5); frozen RKFSIX03 layout with ns times, ctime, volume incarnation, checkpoints (§5); replacement vs link via creation time (§6.1); normalization, aliases, unpaired surrogates (§6.1); identity-map representation and peak (§6.2); directory placeholders not enumerated, coverage reported (§6.2); rescan queue bounds and degraded mode (§6.2); USN probe semantics, frontier, atomic checkpoint, cancellation, `watch=false` rule (§6.2); per-subscriber queues, handshake, retention byte bound, coalescing algebra, persisted ctime (§7); policy views bound across list/search/feed, new-policy and malformed handling (§9.1); link target in disposition (§9.2); page/view/generation binding and cancellation (§9.3); home authorization separated from exclusion discovery, other-users exclusion, subtree revocation purge across all representations (§10); version/format matrix and interrupted-upgrade rule (§11); regression budgets, provider-backed recall, interrupted migration (§12).
