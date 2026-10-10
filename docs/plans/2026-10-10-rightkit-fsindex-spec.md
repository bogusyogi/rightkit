# rightkit-fsindex — specification (v2)

Status: proposed 2026-10-10, revision 2 after the Astra adversarial review (`2026-10-10-rightkit-fsindex-spec-astra-review.md`; 23 findings, all code-level claims verified against source except one). v1 described intent as if it were contract; v2 separates **what the crate does today (0.1.1)**, **what 0.2.0 and 0.3.0 must guarantee**, and **what each consumer must keep**. Decisions marked **[Adrian]** are open. §14 maps each Astra finding to its change.

## 1. What it is

One live file index for the Right Suite: a crawl-once, keep-current catalogue of entries under a set of roots, with fuzzy name search, folder-size rollups that understand hard links and clones, and a change feed, persisted in memory-mapped column files that reopen without a crawl when the store is valid.

Two profiles share one engine:

| Profile | Roots | Consumers | What matters |
|---|---|---|---|
| **volume** | whole disks / home | Pulse | crawl speed, accounting accuracy with stated error bounds, memory per entry, unprivileged upkeep, privacy of the catalogue |
| **workspace** | repository roots | Blueprint, Ledger, CodeRight tools and `@` picker (via adapters, §9) | ignore-policy fidelity, complete and generation-bound listings, a sound change feed |

Non-goals (owned elsewhere): query syntax and FTS compilation (`rightkit-search`), subscriber/debounce watching API (`rightkit-watch`), content ranking and embeddings (Ledger FTS5, Blueprint BM25, `rightkit-embed`), repository graphs and parsing (Blueprint), size reclaim advice, history and presentation (Pulse), file identity definition (`rightkit-fs::identity`, to be widened with fsindex, §6.1). The `content` feature (trigram prefilter) stays opt-in and is not part of any consumer plan.

## 2. Consumers and the contract each keeps

The index **feeds** enumeration and change notification. No consumer deletes its own logic until a parity gate (§12) passes.

| Consumer | Today (verified) | Takes from the index | Keeps |
|---|---|---|---|
| Pulse hub, Mac | `disk_index.rs` on fsindex 0.1.0; `core/src/scan.rs` (identity re-checks, placeholders, reclaim upper bound withheld when incomplete, uncertainty reasons, cancellation; `folder_growth.rs` gap accounting) and `hub/src-tauri/src/scanner.rs` (top-N per folder, name index, `incomplete`/`needs_access`/`unread` flags) | enumeration, sizes, change feed, access/coverage report | reclaim bounds, uncertainty reasons, history/gap accounting, cancellation UX, presentation. Scanner retirement only per §12 parity gate. |
| Pulse hub, Windows | `disk_index.rs` stubs; `watch_windows.rs`; 7-minute per-file walk | 0.2.0 backend | same as Mac |
| CodeRight `@` picker | re-walks per keystroke, 10k cap, `to_lowercase` match on **workspace-relative paths**, files only, highlight indices (`list_files.rs:88-95`, `main_runtime_interactive_session.rs:5089`) | 0.3.0 `search()` over relative paths with match indices (§8) | ranking policy, UI |
| CodeRight tools `list_files`/`search` | `ignore::WalkBuilder` with `git_global(true)` + noise-dir filter + globset; regex over 20k files/page, 60 s deadline | 0.3.0 `list()` through the `coderight` ignore adapter | regex content search, paging, budgets |
| CodeRight repo map | consumes caller-supplied source content; never discovers files (`repo_map.rs:181`) | nothing | — |
| Blueprint | `scan_repository` walks with `read_dir`; **follows symlinks that stay inside the root**, records `dangling_symlink`/`symlink_escape_root` (`graph.rs:448-479`); watch snapshot skips symlinks (`watch.rs:210`); root `.gitignore` + canonical ignored dirs (`graph.rs:57-107`); barrier returns `GapBlocked|CaughtUp|Timeout` with source/applied clocks (`watch.rs:1036,1080`) | 0.3.0 `list()` with link dispositions and the `blueprint` ignore adapter; change feed with cursor + fallible barrier (§7) | graph, parsing, freshness receipts, its own barrier semantics on top of the index cursor |
| Ledger | `walk_markdown` with each directory's own `.gitignore` (no global, no `.ignore`), mandatory exclusions, policy-file byte limits and symlink refusal, `*.md` ASCII-case-insensitive, re-verifies absence before tombstoning (`policy.rs:16-223`, `reconcile.rs:33-50`) | 0.3.0 `list()` through the `ledger` adapter, generation-bound, with coverage metadata | FTS5, policy validation, final absence revalidation |
| `rightkit-search::filename` | in-memory filename index with kind/extension/size filters and offset/limit paging; no users | 0.3.0 provides the same filters and paging; the module retires only when both are proven | — |
| xtool | ripgrep + `fs.readdir` (Node) | deferred | — |
| ViewRight, MailRight, CutRight, GenRight, ScrapeRight, WorkRight | per-document watch or small listings | not consumers | — |

## 3. Architecture

```
rightkit-fsindex
├── core        Index API, worker, change feed (epoch + cursor), rollups, search
├── store       mmap columns, names arena, identity map, header + generation, lock, validation
├── path        lossless platform paths: canonical identity, display spelling, search form  (0.2.0)
├── backend     trait Backend { stat, crawl, list_dir, watch, checkpoint, access }
│   ├── macos   getattrlistbulk + FSEvents (shipped)
│   ├── windows FileIdExtdDirectoryInfo + ReadDirectoryChangesW (+ probed USN replay)      (0.2.0)
│   └── linux   later
├── profile     volume | workspace: ignore adapters, list(), link dispositions              (0.3.0)
└── content     opt-in trigram postings (unchanged)
```

Invariants across backends: a listing's parent precedes its children; links and reparse points are recorded with a disposition and never followed by the crawler; mounts and excluded folders are not descended; hard links are counted once per identity; placeholders are never hydrated by the index (§6.2 states what that does and does not cover); the crawl is unprivileged by default and reports coverage instead of failing.

## 4. API: what exists (0.1.1) versus what is added

Existing, exact (`src/index.rs`, `src/config.rs`, `src/error.rs`):

- `Index::open(cfg) -> Result<Index>`, `open_with_backend(cfg, backend)`: block until the first crawl or a valid reopen; start watcher and worker.
- `status() -> Status { roots, counts, resumed_from_snapshot, full_rescans, last_event_id, full_disk_access, skipped_dirs, unreadable_*, crawl_time }`.
- `search(q) -> Result<Vec<Hit>>` (limit 100; 0 coerced to 1; **basename** match), `search_with(q, &SearchOptions { limit, scope })`.
- `folder_size(path) -> Option<SizeInfo { logical, allocated, allocated_unique, files, is_package }>`; `children_sorted_by(path, SortKey)` where `SortKey::Name` sorts by allocation then name.
- `subscribe() -> Receiver<ChangeBatch>` (bounded 16), `flush()` (returns nothing; gives up silently after 30 s), `snapshot()`, `trim()`, `content_search(needle, limit)` (feature).
- `Item { kind, is_secondary_link, excluded, … }` (no `ext` field). `Config` defaults as documented in `config.rs` (`watch_latency` 50 ms, `change_coalesce` 150 ms, `max_change_paths` 1024, `max_change_rollups` 2048, `crawl_threads` 2..8, `snapshot_max_age` 3 d).
- `Error { Unsupported, Io, State, ContentDisabled, InvalidQuery }`.

0.2.0 (Windows) changes the public surface only by: `Status.access: AccessReport` (§6.4) and `Item.disposition: LinkDisposition` (§9.2). Nothing is removed.

0.3.0 (workspace) adds, all additive:

- `Config.profile: Profile::Volume | Profile::Workspace { adapter: IgnoreAdapter }` (§9.1).
- `list(&ListQuery) -> Result<Listing>`: `ListQuery { under, kinds, glob: &[&str], case: CaseRule, adapter: Option<IgnoreAdapter>, page: PageCursor }`; `Listing { generation, items, next: Option<PageCursor>, coverage: Coverage { complete: bool, unreadable: Vec<PathBuf>, truncated: bool, policy_digest } }`. Stable order (canonical path bytes), pagination, and a `generation` the caller can compare with the feed cursor; an incomplete or stale listing is never returned as complete.
- `search_with` gains `SearchScope::RelativePath` (match over the workspace-relative path with match indices) beside the existing basename scope.
- Feed cursor types (§7): `Cursor { index_id, epoch, seq }`, `Index::since(&Cursor) -> Result<Replay>`, `Index::barrier(timeout) -> Result<Cursor, BarrierError>`.

## 5. Store

Format `RKFSIX02` (0.1.x), per entry 76 B (`c-name c-parent c-first c-next c-prev c-dir` u32; `c-meta c-flags` u8; `c-nlink` u16; `c-mtime c-dev` u32; `c-fileid c-logical c-alloc c-private c-clone` u64), per directory 36 B, names in 16 MiB segments with an on-disk chained hash. The ≈114 B/entry figure is from the synthetic `footprint` bench and must be re-measured on real volumes (§12).

Reopen without crawl today requires: magic, clean header, equal fingerprint (roots, excludes, package extensions), age ≤ `snapshot_max_age`, stored event id ≤ backend's current id. Reopen rebuilds the free list by scanning **every slot** (linear in store size) and link/clone tables from SPECIAL slots. Compaction needs > 50 000 slots, > 25 % free, 600 s since the last. More than 64 roots is an error at `add_root`.

Known defects to fix in 0.2.0 (store hardening, all platforms):

- **Exclusive ownership.** `open_existing` takes no lock; two processes can map and mutate the same store. 0.2.0 takes an exclusive advisory lock on `index/lock` for the lifetime of the `Index`; a second opener gets `Error::State(Busy)`.
- **Validation before trust.** A clean header is followed without structural checks. 0.2.0 validates column sizes against the header, parent/child/sibling link ranges, and name offsets before reopen; failure ⇒ rebuild from crawl, reported in `Status.rebuilt_from: Corruption`.
- **Write-failure state.** `touch()` and the periodic commit ignore `mark_dirty`/`commit` errors. 0.2.0 records a sticky `write_failed` state exposed in `Status`; queries continue, the store is rebuilt on next open, and the feed carries `Rescanned(root)` once.
- **Durable swap.** `swap_store` removes the current directory and ignores rename failures. 0.2.0: rename current→`index.old`, rename `index.new`→`index`, fsync the parent, then remove `index.old`; `recover_swap` handles each interrupted step.
- **Dirty flag durability.** `mark_dirty` msync failure is an error, not ignored.

0.2.0 format `RKFSIX03` adds columns and widens identity (§6.1). Migration rule: an `RKFSIX02` store is **not** reopened by 0.2.0; it is rebuilt by a crawl once (`Status.rebuilt_from: FormatUpgrade`). No dual-format reader.

## 6. Backends

### 6.1 Identity and paths (0.2.0, both platforms)

- Identity becomes `FileId { volume: u64, id: u128 }` stored as `c-vol` (u64) + `c-id-lo`/`c-id-hi` (u64 each); macOS fills `volume` from `st_dev` and `id` from the 64-bit inode zero-extended. Clone keys are namespaced by volume. `rightkit-fs::identity::FileKey` is widened the same way in the same release so Blueprint freshness and fsindex agree.
- Paths: names are stored losslessly as the platform's native bytes (macOS: UTF-8 as returned, no normalization applied to storage; Windows: UTF-16 code units, including unpaired surrogates). Components are split on the platform separator. Three forms are defined per entry: **canonical identity** (what joins and compares, case-folded per the directory's case sensitivity on Windows and per-volume on macOS), **display spelling** (as stored), **search form** (Unicode simple case folding, NFC on macOS, for matching only). Lookups by path go through the canonical form; the old byte-exact `/`-split lookup is removed.
- Windows path spelling: verbatim `\\?\` prefixes and UNC roots are accepted and reported as given; the index never rewrites a root.

### 6.2 Windows backend (0.2.0)

| Topic | Contract |
|---|---|
| Supported platforms | Windows 10 1809+ on NTFS and ReFS; exFAT/FAT32 and SMB volumes are indexed with `access.identity = Unstable` (no hard-link dedup, no USN). Older OS or filesystems refusing `FileIdExtdDirectoryInfo` get `FileIdBothDirectoryInfo` (64-bit id, no reparse tag ⇒ tag read per entry only for reparse-attributed entries); `FileFullDirectoryInfo` is used only when both fail and then marks the subtree `identity = Missing` (no dedup, sizes still correct). A class change restarts that directory's enumeration from scratch. |
| Crawl | one handle per directory, `FILE_FLAG_BACKUP_SEMANTICS|FILE_FLAG_OPEN_REPARSE_POINT`, `GetFileInformationByHandleEx` in 64 KiB pages; no per-file opens. Unelevated. |
| Hard links | `FileIdExtd*` records carry no `nlink`. The store keeps an **identity map** (`volume`, `id`) → slot for every file (not only `nlink>1`); a second occurrence of an identity becomes a secondary link and the first stays owner; ownership hands over on removal as today. Memory budget: 24 B per entry in the map; it is rebuilt on reopen from the columns. |
| Reparse points, placeholders | every reparse-attributed entry stores `c-attrs` (FILE_ATTRIBUTE_* u32) and `c-reparse` (tag u32). Symlinks, junctions, mount points: recorded with `disposition`, not descended. Cloud files (`RECALL_ON_DATA_ACCESS`, `RECALL_ON_OPEN`, `OFFLINE`): the **directory** is descended when its own handle opens without recall (directories are not placeholders); file entries take sizes from the directory record and are never opened by the index. What "never hydrate" covers: the crawl and upkeep never open a file handle. What it does not cover: the opt-in `content` feature reads files and is **disabled for any entry with recall attributes**. |
| Sizes | `logical` = default-stream EOF; `allocated` = default-stream AllocationSize. Alternate streams, compressed/sparse physical usage and ReFS block-clone sharing are **not** measured; `allocated_unique` on Windows is reported with `SizeInfo.unique_is_estimate = true` and equals `allocated` until a ReFS clone probe exists. Consumers must label it. |
| Upkeep | `ReadDirectoryChangesW` per root, 64 KiB buffer (network paths capped at 64 KiB by the API), `FILE_NOTIFY_CHANGE_{FILE_NAME,DIR_NAME,SIZE,ATTRIBUTES,LAST_WRITE}`, re-armed immediately after each completion. **Both** overflow signals are handled: `ERROR_NOTIFY_ENUM_DIR` and a successful completion with zero bytes transferred; either ⇒ `Rescanned(root)` for the whole watched subtree, with changes arriving during the rescan queued and applied after it. Watch failure or root removal ⇒ `AccessReport.watch = Lost(root)` and the root is marked stale until re-crawled. |
| Resume | Capability-probed. At open the backend tries `FSCTL_QUERY_USN_JOURNAL` and a zero-length `FSCTL_READ_USN_JOURNAL`; only if both succeed unelevated is USN replay enabled for that volume. The checkpoint is per volume: `{ journal_id, next_usn }` stored in a new `checkpoints` header section (the single u64 event id stays for macOS). On reopen: journal id mismatch, USN below the journal's lowest valid USN, or any read failure ⇒ recrawl of that volume's roots. Replay runs to completion **before** `open()` returns, with live notifications buffered from the moment the watch is armed (armed before replay starts) and applied after replay — no gap between replay end and live start. `Status.catching_up` is true during replay for consumers that open asynchronously (0.3.0 `open_async`). |
| Elevated path | feature `windows-mft`, off by default: `FSCTL_ENUM_USN_DATA` yields names and ids but **no sizes**; it can only seed the tree; sizes still come from directory records, so the elevated path shortens the crawl only on volumes where directory enumeration is the bottleneck. Measured before being offered. |
| Access report | `AccessReport { denied_dirs, identity: Full|Unstable|Missing per root, watch: Live|Lost, usn: Replay|Unavailable, elevated: bool }` replaces the macOS-flavoured `full_disk_access` bool for both platforms (the bool stays as a derived field). |

### 6.3 macOS (shipped; corrections)

`getattrlistbulk` crawl and FSEvents upkeep as today. `has_full_disk_access` currently returns **true** when the probe paths (Safari, Mail, Messages) are absent or `HOME` is unset; 0.2.0 returns `Unknown` in that case and never assumes access. Clone accounting (§8) is unchanged in semantics but documented honestly.

### 6.4 Linux (later)

Only when a consumer runs there.

## 7. Change feed (0.3.0 rewrite; 0.1.x behaviour stated for honesty)

Today: `seq` is a process-global static shared by every `Index` and restarts at 1; `missed_batches` is reported only when a later send succeeds; pending changes are appended without per-path coalescing; a new directory's contents are stored without `Created` events; a removed directory emits one `Removed` and drops the subtree silently; an edit that keeps size, allocation, private size, clone id, nlink, file id and whole-second mtime is invisible; `flush()` gives up after 30 s without saying so.

0.3.0 contract:

- `Cursor { index_id: u128 (random, persisted in the header), epoch: u64 (incremented on every crawl/rebuild/format upgrade), seq: u64 (monotonic within an epoch, persisted at commit) }`. Every `ChangeBatch` carries `cursor` and `prev_cursor`. A consumer that holds a cursor with a different `index_id` or `epoch` must treat the index as new (full re-list).
- Delivery: the subscription queue stays bounded; when it is full, the index records the gap **in the batch that is dropped's successor** and also exposes `Status.feed_gaps`. `since(&cursor) -> Replay { batches | NeedsRelist }` serves retained batches (retention: last 4096 batches or 15 minutes, whichever is larger) or tells the consumer to re-list.
- Events: per path, the worker coalesces within `change_coalesce` to at most one `Created|Modified|Removed` per path per batch (`Created` then `Removed` cancel out). A new directory emits `Created` for itself and `Rescanned(dir)`; the consumer lists it. A removed directory emits `Removed` for itself and `Rescanned(parent)`; the consumer reconciles. `Rescanned(subtree)` is the only way the index reports "I did not enumerate the delta"; it is never silent.
- Modification detection: the stored mtime gains nanoseconds on both platforms (`c-mtime` widens to u64; part of `RKFSIX03`), and size, allocation, identity and `ctime`/`ChangeTime` participate; same-size, same-second edits are detected via change time. Content-sensitive invalidation remains the consumer's job (Blueprint hashes).
- Barrier: `barrier(timeout) -> Result<Cursor, BarrierError::{Timeout(cursor_so_far), Gap(cursor)}>` applies every event the backend had delivered before the call and returns the applied cursor. Blueprint keeps its own `GapBlocked|CaughtUp|Timeout` and source/applied clocks on top; the index never claims consumer freshness.

## 8. Search and rollups (stated as implemented)

- Query: tokens ANDed; plain words are fzf-style subsequence matches over **ASCII-folded bytes**; `'x` substring, `^x` prefix, `x$` suffix, `!x` exclude; tokens > 128 bytes are an error; an empty or exclusion-only query is an error. One typo for tokens of ≥ 5 **bytes**, not at the first character, at the start of the name or of a space-separated word (cost 60); digits never count as typos. 0.3.0 adds the search form from §6.1 (Unicode simple folding, NFC on macOS) and `SearchScope::RelativePath` with match indices.
- Rollups, as the code does them: hard links have **one global owner**; the folder holding a secondary link gets neither its bytes nor its file count. Clone groups charge every containing folder the group-wide largest shared part, even where that folder's member is smaller. `allocated_unique` is therefore a global-attribution figure, not folder-local uniqueness, and it is not additive across siblings. 0.2.0 documents this on `SizeInfo` and adds `SizeInfo.files_linked_elsewhere` so a folder can show "n files counted under another folder". **[Adrian]** keep global attribution (fast, stable) or move to per-folder distinct accounting (each folder counts every identity it contains once; costs an identity set per rollup update).
- `content` feature: unchanged; disabled for recall-attributed entries (§6.2).

## 9. Workspace profile (0.3.0)

### 9.1 Ignore adapters, not an enum

Consumers' policies differ in ways an enum cannot express: CodeRight uses global git ignores plus a noise-directory filter; Blueprint uses the root `.gitignore` plus canonical ignored directories; Ledger uses each directory's own `.gitignore` only, with mandatory exclusions and policy-file validation. 0.3.0 ships `IgnoreAdapter` as a trait with three built-in, versioned adapters (`coderight`, `blueprint`, `ledger`) that reproduce those rules exactly (including ancestor pruning), each with a `policy_digest` that changes when any policy file it read changes; a digest change invalidates listings and emits `Rescanned(root)`. The store keeps every entry; adapters filter at list/search/feed time so one index serves all three. Hard exclusions (`.git` directories **and** `.git` gitfiles, nested repositories, worktrees) are applied per adapter, not globally.

### 9.2 Links

Every entry records `disposition: None | Symlink{ target_within_root: bool, dangling: bool } | Junction | MountPoint | Placeholder`. The crawler never follows links. Blueprint, which today follows confined internal links, gets the target path and disposition and resolves what it needs itself; its watch snapshot's "skip symlinks" and graph scan's "follow confined" disagreement is Blueprint's to settle, not the index's.

### 9.3 Listings

`list()` is generation-bound, paginated, in stable canonical order, and carries `Coverage`; `**/*.md` matching honours `CaseRule::AsciiInsensitive` for Ledger. Ledger's final absence revalidation and CodeRight's paging budgets stay in the consumers.

## 10. Privacy and permissions

- Store directories and files are created with owner-only permissions (`0o700`/`0o600` on Unix; an owner-only DACL on Windows) and the state directory is excluded from its own index.
- Roots are explicit; the `home_dirs`-unset behaviour of indexing every `/Users/*` becomes opt-in (`Config.discover_homes = false` by default).
- Access state is tri-state (`Granted|Denied|Unknown`); the index never assumes access. `allow_consent_prompts` is documented as "may trigger TCC prompts by attempting access", not as authorization.
- Revocation: when a previously readable root becomes unreadable, its entries are purged at the next commit and `Rescanned(root)` is emitted; `Config.retain_on_revocation = false` by default.
- Diagnostics redact names under protected roots; `Status` never includes paths from denied directories beyond their count.
- One store per OS user; shared/multi-user volumes are indexed per user.
- Removable volumes: identity includes the volume serial; a volume that returns with a different serial is a new epoch for its roots.

## 11. Versions

| Version | Content | Gate |
|---|---|---|
| 0.1.1 | leftover 0.1.0 polish (shipped 2026-10-10) | — |
| 0.2.0 | store hardening (§5), identity/path model (§6.1), `RKFSIX03`, Windows backend (§6.2), macOS access tri-state, privacy defaults (§10), honest rollup docs | Windows and macOS qualification (§12); Pulse parity gate before any scanner retirement |
| 0.3.0 | change feed rewrite (§7), workspace profile with adapters (§9), `list()`/`since()`/`barrier()`, relative-path search, `rightkit-fs::FileKey` alignment | consumer parity gates for CodeRight picker/tools, Blueprint, Ledger |
| later | Linux; ReFS clone probe; `windows-mft` if measured useful | — |

## 12. Qualification (release gates, measured, recorded under `release-evidence/`)

- **Platform matrix:** macOS APFS (clones, hard links, packages, FDA denied/granted/unknown); Windows NTFS and ReFS (hard links via `CreateHardLinkW`, junctions, symlinks, long paths, OneDrive placeholder fixture with recall attributes, compressed and sparse files), exFAT and SMB (identity `Unstable`), removable volume replaced between sessions, non-admin only.
- **Upkeep:** RDCW overflow via `ERROR_NOTIFY_ENUM_DIR` **and** zero-byte completion; USN replay with journal-id mismatch and out-of-range USN; same-size same-second edit; concurrent second opener refused; corruption injected into each column class ⇒ rebuild.
- **Performance:** cold crawl, warm reopen, catch-up latency, query p50/p99 and update latency, **peak** working set and private bytes (not sampled), crate build size and dependency count; on the Dell home folder on `C:` (≈2M entries, 7-minute baseline) and on the Mac. Targets are set from the first measurement, not asserted in advance.
- **Consumer parity gates** (each a test in the consumer repo, run by that app's chat): Pulse — identical folder sizes, uncertainty reasons and reclaim bounds between scanner and index on a fixture volume, cancellation within 200 ms; CodeRight picker — identical result sets and highlights versus the walk on the same workspace; Blueprint — identical file sets, link dispositions and barrier outcomes on its fixture repos; Ledger — identical markdown sets under its policy fixtures including malformed-policy rejection.
- **Tests:** platform-specific suites are counted per platform (today 17 macOS integration + 3 content + 1 burst + 2 unsupported + 3 query); Windows gets its own integration suite of at least the same scenarios where the platform has an equivalent, plus the items above.

## 13. Open decisions

1. **[Adrian]** Go on 0.2.0 (store hardening + identity model + Windows backend); needs the Dell for build, tests and measurement.
2. **[Adrian]** Rollup accounting: keep global attribution with honest labelling, or per-folder distinct accounting (§8).
3. **[Adrian]** Workspace profile in fsindex (0.3.0) versus each consumer keeping `ignore::WalkBuilder`. Recommendation unchanged (fsindex), now with adapters and parity gates rather than an enum.
4. `rightkit-search::filename` retires only after 0.3.0 proves kind/extension/size filters and paging.

## 14. Changes from v1 (Astra finding → change)

1 API inventory corrected (§4). 2 search restrictions stated (§8). 3 rollup semantics stated as global attribution; decision 2 (§8). 4 store limits stated (§5). 5 lock, validation, durable swap, write-failure state (§5). 6 identity widened, `RKFSIX03` rebuild-only, `FileKey` aligned (§6.1). 7 OS/filesystem qualification and class fallback (§6.2). 8 identity map for all files (§6.2). 9 lossless path model (§6.1). 10 placeholder matrix and content exclusion (§6.2). 11 size coverage and `unique_is_estimate` (§6.2). 12 zero-byte RDCW completion (§6.2). 13 probed USN, per-volume checkpoints, replay before open (§6.2). 14 cursor with index id + epoch, retention, `since` (§7). 15 coalescing, subtree events, change-time detection (§7). 16 fallible barrier with cursor (§7). 17 ignore adapters with digests (§9.1). 18 generation-bound paginated listings with coverage (§4, §9.3). 19 link dispositions, Blueprint's inconsistency named (§9.2). 20 Pulse parity gate instead of deletion (§2, §12). 21 consumer rows narrowed; filename module retirement conditional (§2). 22 privacy section (§10). 23 measured gates, peak memory, platform matrix (§12). The one refuted sub-claim (65 roots silently dropped) is not adopted: `add_root` errors at 64.
