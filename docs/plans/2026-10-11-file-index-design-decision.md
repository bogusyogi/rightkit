# File index design decision (2026-10-11)

Status: Sage adjudication for Adrian and the RightKit lead. Advice only, no code. It answers gap ledger #76 and #61 (Windows backend part) and spec §13 items 1 and 3.

## 1. Decision in one paragraph

**The index stays a RightKit-owned Rust crate (option A), but it is rebuilt on the OS change primitives, not on the OS query index.**

- **Primitives:** getattrlistbulk, FSEvents with a stored event id, and on Windows the NTFS file table and USN journal where available.
- **Not a base:** Spotlight and Windows Search fail on the evidence below and are not a base.
- **No source split:** the hybrid (D) is rejected as a split by source (OS names plus our sizes). Its catalogue half is what A already is.
- **Engines:** no engine is adopted as a dependency, binary or parent path (option C). Their designs are vendored as ideas: Everything's journal restart, plocate's on-disk layout and ctime-skip, and Cardinal/FSearch's named-item updates.
- **Scope cut:** the "one index for every profile" ambition is cut. The workspace profile (0.3.0: policy views, feed cursors, listings) is deferred, because it has no consumer today.
- **Contract:** the volume profile ships only under a runtime budget that pairs every performance figure with a correctness figure (§7).

## 2. What the evidence says

**What failed in 0.1.0**

| Fact | Source |
|---|---|
| 20.7 M entries (12.58 M on `/`, 8.10 M on `/Volumes/D`), 5.4 GB resident, about 220 B/entry | Survey-Mac; ledger #76 |
| 12–15 k small disk operations per second and 65% CPU from re-listing build folders | ledger #76 |
| Cause of the churn: watch latency 50 ms, and each directory-level event triggers a full `getattrlistbulk` re-list | `index.rs` `handle`→`sync_path`; README "Live updates" |
| First crawl of D took 183 s at 500% CPU | ledger #76 |

**Two defects that 0.2.0 does not remove**

1. **Re-listing is still the update mechanism.** 0.2.0 only rate-limits it. The README confirms the index still runs on "FSEvents (directory granularity) drives per-directory getattrlistbulk diffs". The survey finds no other tool that re-lists per change. FSearch, Cardinal and Everything update the one named item. Survey-Mac says the per-folder backoff has no source and is our own invention.
2. **Any dropped-event flag triggers a whole-volume rebuild.** `index.rs:1179-1198` sets `full` on `EV_USER_DROPPED` or `EV_KERNEL_DROPPED` and runs `full_rescan`, which calls `build_fresh` and swaps the whole store. At 12–15 k events/s, kernel drops are likely. The default response to a dropped event should be a subtree sweep that skips folders whose ctime is unchanged (plocate's method, Survey-Mac), not a 20 M-entry rebuild.

**The 220 B/entry figure has two readings**

- The 76 B of per-entry columns on disk plus names, directory columns and tables gives about 114 B/entry (README, measured synthetically at 7.7 M).
- The remainder is resident file-backed pages, plus the heap tables `links: HashMap` and `clones: HashMap<_, Group{Vec, HashMap}>`.
- 0.1.0 had no `trim`. The 0.1.1 synthetic run shows 24 MB footprint at rest for 7.7 M entries, with 185 MB resident after a name search.
- **No real 20 M-entry run of 0.1.1 or 0.2.0 exists in the repo.** The `budget` bench exists, but the README has no "Runtime budget" section yet, and the CHANGELOG itself says the compact representation "was not changed".
- The memory half of #76 is therefore partly a measurement and missing-trim effect, which nobody has measured. The CPU half is a real design defect.
- Any new budget must report phys_footprint, anonymous memory, and file-backed resident memory separately.

**The columns are heavier than the work needs**

- RKFSIX02 carries 76 B for every entry, files included. That includes `c-first`, `c-prev`, `c-dir`, an 8 B file id, and three 8 B sizes plus a clone id.
- RKFSIX03, as frozen in spec §5, grows that to 108 B so that Windows can share the layout. At 20.7 M entries that is about 2.2 GB of columns before names.
- The comparison points are Everything at about 100 B resident, FSearch at about 60–80 B (derived by Pulse), plocate at about 17 B on disk, and the Dell chat's unsourced 40–60 B estimate (Survey-Mac and Survey-Win).
- FSearch's smaller footprint is not a different primitive. Its crawl and FSEvents code are what this crate already adapted (`THIRD-PARTY-NOTICES`). The README says "Unlike upstream this crate keeps 64-bit logical and allocated sizes, rolls folders and packages up, and dedupes hard links and clones". The gap is therefore representation and update policy, plus the extra accounting we added.
- **Do not copy FSearch's numbers.** The new content-search comparison against fff (58–86% recall, source unverified) shows its figures are self-measured, and that its filter may lose real matches. That comparison is content search, not name search, so it does not transfer directly. I take only the rule: a performance figure without a correctness figure is not evidence.

**Who consumes this today**

- Only Pulse hub on Mac, pinned `=0.1.0` (`hub/src-tauri/Cargo.toml:38`), and switched off on this Mac with `PULSE_NO_DISK_INDEX=1`.
- Pulse uses two calls: `search_with` for Storage name search, and `folder_size` for given paths.
- The Windows hub has no index and answers with a plain scan: 2.77 M entries in about 8.5 min at 1.0–1.3 GB peak (Survey-Mac).
- Pulse already has its own scan index with live refresh, entry budgets, and an idle unload after 120 s (`scanner.rs:118,495,888`), plus its own uncertainty and reclaim logic (`core/src/scan.rs`).
- CodeRight, Blueprint and Ledger do not depend on fsindex. The consumer table in spec §2 describes planned adapters, not current use.

## 3. Requirements matrix per consumer

"Must" means a failure here blocks adoption. Each cell shows what the consumer needs from the index.

| Requirement | Pulse Storage (volume) | CodeRight `@` picker and tools | Blueprint | Ledger |
|---|---|---|---|---|
| Name search | Must: whole disk, ranked, fuzzy. Today's UX needs about 100 ms. | Must: relative-path match with highlights, files only | No | No |
| Folder-size rollups | Must: logical, allocated, unique; hard-link and clone aware; stated error bounds | No | No | No |
| Change feed | Should: live refresh. Loss must be signalled, never silent. | Should | Must: gap-aware cursor and barrier | Must: view-bound generation, absence revalidation |
| Privacy and exclusions | Must: protected folders, other users' homes, state dir self-excluded, subtree revocation purge | Must: gitignore, global ignore, noise dirs | Must: root gitignore plus canonical dirs, confined links | Must: per-directory gitignore, mandatory exclusions, policy byte checks |
| Unprivileged operation | Must on Mac. On Windows: preferred, with an optional elevated tier. | Must | Must | Must |
| Memory and CPU | Must: budget in §7 | Trivial (10⁴–10⁶ entries) | Trivial | Trivial |
| Windows parity | Must (Pulse ships on Windows) | Must | Must | Must |

Two points follow from the matrix:

- The workspace consumers need hidden folders, `.gitignore` semantics, and exact policy snapshots. No OS index provides any of those. Their data is small, so memory does not matter for them.
- Only Pulse's volume case is hard, and it is the only live consumer.

## 4. What the OS indexes cannot provide

| Needed | Spotlight and Windows Search | Consequence |
|---|---|---|
| Coverage of the scopes we need | Not met on the machines tested. Mac: indexing disabled on `/Volumes/D`, so an existing file is not found. `~/.claude` has 303 JSON files and 0 are found. `/usr/bin` has 932 items and 0 are found. Dell: 0 of 210 `.rs` files in the Pulse repo, and no `Cargo.toml` anywhere. The user controls the scope. (Survey-Mac, Survey-Win) | Not a base. Changing the scope (`mdutil -i on`, Windows Search scope settings) edits user system configuration and triggers heavy indexing, so we must not do it. |
| Folder sizes | `kMDItemFSSize` is null for folders (Survey-Mac) | We must roll sizes up ourselves. |
| Hard links and clones | No identity or private-size data (`ATTR_CMNEXT_PRIVATESIZE`, clone id come only from getattrlistbulk) | Our catalogue. |
| Excluded dirs and ignore policy | Cannot express `target` or `.gitignore` rules | Our catalogue. |
| Event replay and completeness | No cursor we control. No source found on how Spotlight consumes FSEvents (Survey-Mac). | Our event-id and USN-id replay. |
| Access coverage report | Silent privacy filtering. A query under `~/Documents` returned nothing and the reason was not established. | Our access report. |

**Does this force option D? No.**

- D would use the OS index for name search. That half fails on the measured coverage above, so there is nothing for D to combine.
- A split would also give search results and sizes two sources of truth with different coverage, which makes any parity gate meaningless.
- What the OS does give us is its change primitives: FSEvents with a stored id, the USN journal, and bulk listing. Those are the correct substrate. Spotlight stays a Pulse-side launcher feature (`LauncherFileSearch.swift`, parity row H3), not RightKit's index.

## 5. Options scored

Scores: **+** meets, **~** partial, **−** fails. Evidence is in §2 to §4.

| Criterion | A: own index, 0.2.0 plus Windows backend | B: OS index | C: adopt an engine | D: OS names plus own catalogue | **Recommended: A rebuilt (A′)** |
|---|---|---|---|---|---|
| Name search, whole disk | + (already built) | − (coverage) | ~ | − (coverage) | + |
| Folder rollups, hard links and clones | + | − | − (none provide them) | + | + |
| Change feed and replay | ~ (re-list design) | − | ~ | ~ | + |
| Privacy and exclusions | + | − | ~ | ~ | + |
| Unprivileged | + on Mac. On Windows, conflicts with a MFT approach. | + | − (Everything needs admin or a service) | + | + base tier, optional elevated tier |
| Memory and CPU | − as built (re-list, 76–108 B columns, full rebuild on drop) | + | ~ (unverified self-measured) | ~ | + (target, proven by §7) |
| Windows parity | − (stub; spec §6.2 is a large unbuilt surface) | − (scope) | − | − | ~ (rung 3 baseline; rung 1 optional) |
| Ownership and licence | + RightKit-owned | + (system API) | see below | + | + |

**Licence and ownership for option C**

- **Everything:** closed source. It redistributes legally, but its SDK is IPC to a running Everything, which means a second resident process and the same admin install. Its Rust wrapper crate is GPL-3.0, and the SDK licence is unverified (Survey-Win). WizFile, UltraSearch and SwiftSearch are admin-only, freeware or abandoned, and not embeddable. It cannot be a dependency, and we adopt its journal-plus-snapshot-restart design only.
- **FSearch (noahdunnagan, MIT, Rust, macOS):** we already adapted the crawl, FSEvents binding and scorer under MIT with a notice (`THIRD-PARTY-NOTICES`). Nothing more is gained from adopting it as an engine, and its figures are self-measured.
- **FSearch (cboxdoerfer, Linux, C):** a different project from noahdunnagan's. Survey-Mac found its licence unchecked, and from memory I believe it is GPL, which I have not verified. It is learn-from only.
- **plocate and watchman:** design only. plocate is on-disk trigram postings with no live tracking. Watchman is not a disk index and is not persistent.
- **Cardinal:** a Mac app, not a library, with its licence unchecked (Survey-Mac). Learn-from only.
- **Crates to read for the Windows work:** usn-journal-rs 0.4.1 (MIT) and ntfs-reader 0.6.0 (MIT or Apache-2.0), both read-first candidates as dependencies, subject to the ownership allowlist, which is Adrian's trust-sync (Survey-Win).

## 6. Recommended design (A′)

**Substrate and update policy**

1. **Named-item updates.** Use file-level FSEvents with a latency of 1–3 s (Apple's guide: longer latency reduces volume; its example is 3 s). A batch reduces to covering paths (Cardinal's method). Each named item is handled with a single lstat or stat. No directory is listed per event. Listing a directory remains only for "must scan subdirs", root-changed, mount, new directory, and dropped events.
2. **Dropped events become sweeps, not rebuilds.** Do a subtree sweep that skips folders whose ctime is unchanged (plocate). A whole-store rebuild is allowed only when the event id history is unusable. The existing re-list rate limiter stays as a governor on sweeps, not as the main fix.
3. **Hot-directory shedding.** macOS filters only the first 8 ignore paths itself (Survey-Mac, citing Watchman). Events under excluded folders still arrive, so drop them by prefix check before any I/O, and sample per-top-level event rates. Add the hottest paths to the stream's exclusion list, up to 8.
4. **Default excludes.** Keep the 0.2.0 set. No surveyed tool excludes `target` or `node_modules` by default (Survey-Mac). That is our own choice, and Watchman's advice supports it. Cardinal's list (`/Volumes`, `~/Library` caches, logs and cloud storage, `/private/var`, `/private/tmp`) is a candidate for the Pulse root-volume profile. Document it as policy, not as a copy of an unlicensed source.

**Representation (volume profile only)**

- Split files from directories. Files drop `c-first`, `c-prev` and `c-dir` (about 12 B), and carry a 4 KiB-unit u32 for allocated bytes, with overflow in a side table.
- Move file id, private size and clone id to the side tables that already exist for hard-linked and cloned entries (the `SPECIAL` flag).
- **Derived target (not measured):** about 28–32 B of columns plus 12–20 B of names, so 45–55 B/entry on disk. That is about 1.0–1.1 GB for 20.7 M entries, with near-zero footprint at rest, because the columns are file-backed.
- Keep the mapped columns (the one thing 0.1.1 got right). Bound transient search residency with a bounded scan and a `trim` after search.
- Bound the `links` and `clones` heap tables, and report their size in `status()`. A pnpm- or Xcode-style store can make them large.

**Lifecycle (a product choice for Pulse, not the crate)**

- Build on first use of Storage search, not at hub start. Run the crawl at background priority with 2 threads, not 500% CPU.
- Idle unload, as the scan index already does after 120 s. On wake, replay from the stored event id. If history is not intact, do the ctime-skip sweep.
- Every state is reported through `status()`: live, catching up, stale, shed. Never leave search silently stale.

**Windows (replaces spec §6.2 as the plan)**

- **Baseline, rung 3:** bulk walk (`FileIdBothDirectoryInfo` or `FileIdExtdDirectoryInfo`) plus `ReadDirectoryChangesW`. It needs no admin and also covers FAT, exFAT, network and removable volumes, but it is lossy by design: subtree rescan on overflow (Survey-Win).
- **Optional rung 1, elevated helper:** `FSCTL_ENUM_USN_DATA` plus `FSCTL_READ_USN_JOURNAL`, with the journal id and next USN stored per volume. Rescan fully on id change or lost range. The Dell's C: journal is 32 MB at most, so a build can wrap it. Whether to ship an admin-installed service is Adrian's decision, because the cost is the same one Everything pays (Survey-Win).
- **Rung 2 (unprivileged journal read):** treat as unverified until tested. Microsoft documents all journal operations as admin-only. A journal query answered unelevated on the Dell, but reading is untested, and the call that would read it unprivileged is undocumented with names stripped.
- **No Windows backend is built before the Dell tests in §9 are done.**

**What stays out of the volume crate:** policy views, listings, `PolicyChanged`, cursor epochs and retention. They are workspace-profile features. See §8.

## 7. Runtime budget the recommendation commits to

These are commitments. Every row is a gate and must pass in the named run before the volume profile publishes or Pulse re-enables it (`docs/RUNTIME-BUDGETS.md`). All figures use the same run. Performance targets below are derived, not yet measured.

**Performance, per run on a real volume of at least 8 M entries (the Mac `/` and `/Volumes/D`)**

| Metric | Commitment |
|---|---|
| Footprint at rest after open | ≤ 4 B/entry, so ≤ 100 MB at 20.7 M |
| Footprint under churn, steady state | ≤ 250 MB at 20.7 M (about 12 B/entry) |
| Resident (RSS) peak during one name search | ≤ 40 B/entry, released by `trim` within 30 s |
| On-disk size | ≤ 55 B/entry |
| Steady CPU under the reference churn (existing `budget` bench: 400 changes/s, 60 s) | ≤ 2% of one core averaged |
| Peak CPU, any 5 s window | ≤ 25% of one core, enforced by a self-throttle that reports `shed` instead of exceeding it |
| Disk operations per second attributable to the index under the churn | ≤ 50 |
| Crawl | ≤ 2 threads, background priority. Report time (183 s at 500% CPU is the 0.1.0 baseline). |
| Reopen | Report cold, warm, and catch-up time |
| Query | Report p50 and p99 |

Footprint, anonymous memory and file-backed memory are reported separately. `status()` exposes them, as 0.2.0 already starts to do.

**Correctness, required beside every row above (Adrian's instruction of 2026-10-11)**

Each correctness figure is measured against a ground-truth full scan of the same roots, with the same excludes, taken with a plain tree walk.

| Figure | Method | Commitment |
|---|---|---|
| Entry recall and precision | Quiesce the volume, then diff the set of paths in the index against the ground-truth scan | Recall 100% and precision 100% on a quiescent tree. Any entry lost under churn must be reported as a bounded, counted gap (`stale` or `shed`). |
| Size-rollup accuracy | Compare `logical`, `allocated`, `allocated_unique` and file count per folder (all top-level folders, plus a random 1,000-folder sample) against Pulse's scan (`core/src/scan.rs`) | Exact for logical, allocated and counts. For `allocated_unique`, the clone estimate is labelled and its error is reported. |
| Change-feed completeness under churn | Run the existing churn bench. After the churn stops, measure convergence time and any missing or extra entries versus the ground truth. Also inject dropped events, must-scan-subdirs and a history gap. | Zero missing entries after convergence. Convergence ≤ 30 s for the bench. Every gap path ends in a counted, visible rescan. |
| Search recall | For a set of known-name queries, confirm the index returns every ground-truth path that should match, excluding documented excludes | 100% |

A single result without the matching correctness figure fails the gate. FSearch's published speed and memory figures stay unverified, and no budget row is copied from them.

## 8. Migration and consumers

**Pulse hub (Mac).**
- It stays pinned at `=0.1.0` and off (`PULSE_NO_DISK_INDEX=1`) until A′ passes §7.
- It then moves to the new version in one step, with lazy start and idle unload, and a status line showing the §7 figures. Its scanner, uncertainty and reclaim logic stay in Pulse.

**Pulse hub (Windows).** No change now. It keeps the plain scan and `ReadDirectoryChangesW` until the Windows tests in §9 are done. This is not a regression, because the Windows hub has never had an index.

**CodeRight `list_files` and picker, Blueprint, Ledger.**
- They do not use fsindex today, and nothing migrates.
- Defer the workspace profile (0.3.0): the policy views, feed cursors and listings (spec §7 and §9). Revisit when a consumer shows measured pain.
- The one evidenced pain is the picker's per-keystroke walk with its 10 k cap. It can be addressed without fsindex: `rightkit-search` already ships an in-memory filename index (its Cargo description), fed by the walk the picker already runs.
- This answers spec §13 item 3 as: per-consumer walkers for now, not the workspace profile in fsindex.

**Public APIs.**
- `list_dir_fd` and `FsEventsWatcher` (ledger #61) are design-independent and Pulse asked for them. They may publish on their own once the lead decides how to package them.
- The index half of 0.2.0 does not publish until §7 passes. Any version bump must not freeze a layout in `RKFSIX03`.

## 9. What to stop doing in the 0.2.0 lane

1. **Stop freezing `RKFSIX03` at 108 B/entry.** Ledger #76 already says the format is not frozen. The Windows-driven fields (128-bit id, 64-bit volume, ns times, ctime, attrs, reparse) belong in a Windows variant or side tables, not in every Mac file entry.
2. **Stop building the Windows backend** (spec §6.2: FileIdExtd enumeration, identity map, USN replay probe, cloud-placeholder rules) until the Dell tests are run and Adrian decides on an elevated helper.
3. **Stop the spec §7, §9 and §10 expansion for the volume crate.** That means cursors with epochs, per-subscriber queues, policy views, listings and `PolicyChanged`. The §10 privacy items that Pulse needs stay: private store, subtree-revocation purge, other-users exclusion, and self-excluding the state directory.
4. **Stop treating the per-folder re-list rate limiter as the fix.** Keep it as a governor, and stop tuning its intervals.
5. **Stop quoting a synthetic 1 M-entry figure as the budget.** A real-volume run is required.
6. **Do not add the `content` feature to any plan.**

**Order of work for the lane:** named-item updates, then drop handling by sweep, then compact columns, then lifecycle, then the real-volume budget run. Publish nothing from the index half before the last step.

## 10. Open items needing Adrian

1. **Does Pulse need a whole-disk name index at all, or only the folders Storage shows?** The Windows hub works without one, and the Mac hub already has a budgeted scan index. If the answer is "only the shown folders", the volume profile can be smaller than this document assumes. This is a product call, and the design above holds either way.
2. **Elevated Windows helper:** whether to ship an admin-installed service for rung 1, as Everything needs.
3. **Dell tests (local builds or scripts needed there):** whether an unprivileged user can read the journal, whether a normal user can open a non-system volume for enumeration, and bulk enumeration speed. Both Dell volumes are NTFS, so rungs 2–4 cannot be tested on other filesystems there.
4. **Crates allowlist:** adding usn-journal-rs or ntfs-reader is a trust-sync item.
5. **Publishing the `list_dir_fd` and `FsEventsWatcher` primitives** ahead of the index (§8).

**Not verified in this document:** the noahdunnagan FSearch README (not fetched); licences of cboxdoerfer FSearch, Cardinal, and the Everything SDK; the Dell chat's 40–60 B/entry estimate; and the fff-versus-FSearch recall table (Adrian's source, unverified).
