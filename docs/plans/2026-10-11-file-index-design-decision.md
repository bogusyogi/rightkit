# File index design decision (2026-10-11)

Status: Sage adjudication, advice not a contract. Decider: Adrian. Resolves gap-ledger #76, and spec §13 items 1 and 3.

## 0. Decision in five lines

1. **Option A, narrowed and re-founded.** RightKit keeps one Rust crate, `rightkit-fsindex`. It is built on the OS change journals (FSEvents, NTFS USN). It is not built on an OS query index (Spotlight, Windows Search). Neither can serve this job (§3).
2. **No hybrid by data source.** The OS-index half of D covers the cheapest part and has measured coverage holes, so a hybrid would give two inconsistent sources of truth. The principle behind D is kept at the primitive level: change journals and bulk listing are the OS facilities we use.
3. **No engine is adopted.** FSearch's Rust/macOS crawl, FSEvents binding and scorer are already vendored here under MIT. Everything is closed and IPC-only. plocate and Everything are design references only (§6).
4. **The volume profile and the workspace profile split.** Volume is Pulse's whole-disk catalogue and is the only one with a memory or CPU problem. Workspace (Blueprint, Ledger, CodeRight) has no consumer on fsindex today. Its 0.3.0 work is deferred (§7).
5. **The 0.2.0 lane stops freezing RKFSIX03 and stops the Windows backend.** It re-bases on named-item updates, a compact volume record and a governor (§8).

## 1. Evidence base and its limits

- **Measured, 0.1.0 in the Pulse hub on Adrian's Mac** (ledger #76; Pulse survey):
  - 12.58 M entries on `/` and 8.10 M on `/Volumes/D`, 5.4 GB resident, about 220 B/entry.
  - 12–15k small disk operations per second and 65% CPU, from a 50 ms FSEvents latency with a full folder re-list per changed folder.
  - First crawl of D: 183 s at 500% CPU.
- **0.1.1 numbers are synthetic, not from a real volume** (README): at 7.7 M entries, 836 MB on disk, 24 MB footprint at rest and about 185 MB resident after a search.
  - So the 5.4 GB mixes two things: file-backed mapped column pages (0.1.0 had no `trim`) and real heap.
  - No real-volume figure exists for 0.1.1 or the 0.2.0 working tree. The `budget` bench exists, but its results are not recorded anywhere in the tree.
  - The README has no "Runtime budget" section yet, although the changelog cites one. The release gate (`packages/release`) will refuse publication without it.
- **Memory and CPU have different causes.** CPU and disk load is a policy defect. Events are directory-granular, and each one triggers `sync_path`, a full `getattrlistbulk` of that directory (`index.rs:1202-1206`, `1345`). Memory is representation plus the absence of trimming.
- **A second unfixed hazard.** Any `USER_DROPPED` or `KERNEL_DROPPED` flag triggers `full_rescan`, which rebuilds the whole store (`index.rs:1179-1197`, `1291`). Under build churn, kernel drops are likely. Nothing in 0.2.0 addresses this.
- **Per-entry columns, as built:** 76 B in RKFSIX02 (`store.rs:93-108`). Files carry `first`, `prev` and `dir`, which only directories need. They also carry 64-bit `private`, `clone` and `fileid`, which only hard-linked or cloned files need. The spec's frozen RKFSIX03 grows this to 108 B, mostly for Windows fields. The `links` and `clones` side tables are heap `HashMap`s, and each clone group holds a `Vec` plus a `HashMap`.
- **Sources cited.** "Pulse survey" is `/Volumes/D/claude/pulse/docs/file-index-survey.md` (Mac and cross-platform, plus the Windows section from the Dell chat). Where I cite it, I'm citing its links and its own labels (derived, unverified), not independently re-checking them.
- **Pulse today already has a budgeted scan tier.** The folder index has live refresh, idle unload after 120 s, and a 2 M entry / 500k-per-directory budget (`hub/src-tauri/src/scanner.rs`). The fsindex is an accelerator for name search and folder-size lookups (`disk_index.rs` `search` and `sizes`). The Dell hub works with no index: a plain scan of 2.77 M entries takes about 8.5 min and peaks at 1.0–1.3 GB.
- **Only one consumer is on fsindex today.** The Pulse hub on Mac pins `=0.1.0` (`pulse/hub/src-tauri/Cargo.toml:38`). CodeRight, Blueprint and Ledger do not depend on it; the spec's table describes planned adapters.

## 2. Requirements matrix

| Requirement | Pulse volumes (Mac, Win) | Blueprint / Ledger / CodeRight (workspace) |
|---|---|---|
| Name search | Whole-disk fuzzy name search, ranked, ≤100 hits, extension filter | CodeRight picker: relative-path match with highlights; tools: list/glob |
| Folder-size rollups | Required: logical, allocated, allocated-unique; hard links once; clone-aware (APFS) | Not needed |
| Change feed | Needed internally for upkeep; a subscriber API is a nice-to-have | Required and strict: cursors, replay, barrier, policy views |
| Privacy / exclusions | Protected-folder and other-users exclusion; default caches/build output; revocation purge | Ignore-policy fidelity: per-directory `.gitignore`, global ignores, mandatory exclusions |
| Hidden and system dirs | Required (`/usr`, `~/.claude`, other volumes) | Required (`.git`, dotfiles) |
| Unprivileged operation | Required on Mac; on Windows, preferred | Required |
| Memory / CPU budget | Hard: 20 M+ entries, hub shares the machine with builds | Soft: 10^4–10^6 entries |
| Windows parity | Needed (Dell hub has no index today) | Needed |

## 3. Options scored against the matrix

Ratings: **Y** meets, **P** partial, **N** fails.

| | A: own index, 0.2.0 as planned | A′: own index, re-based (recommended) | B: OS index | C: adopt engine | D: hybrid |
|---|---|---|---|---|---|
| Name search, whole disk | Y | Y | **N**: off on `/Volumes/D`; no hidden or system dirs (`~/.claude` 303 json files, 0 found; `/usr/bin` 932 items, 0 found); Windows Search: D: and repos out of scope, 0 of 210 `.rs` in the Pulse repo | P | N for the same coverage reason |
| Folder-size rollups | Y | Y | **N**: `kMDItemFSSize` is null for folders; hard-link and clone accounting impossible | N: FSearch has none by our own README note ("unlike upstream… 64-bit sizes, rollups, dedupes hard links and clones"); Everything has no rollup API | Y (own catalogue) |
| Hidden/system, excluded dirs | Y | Y | N | P | P |
| Event replay | Y on Mac; Windows planned | Y (stored event id; USN) | N: no public replay | N | Y |
| Unprivileged | Y Mac / P Windows | same | Y | Everything: service or admin | Y |
| Memory/CPU | **N** as measured; 0.2.0 does not change the representation | **Y if the §9 budget is met** | Y (none of ours) | P | P (catalogue still carries the cost) |
| Windows | Stub; spec's backend is large | Ladder in §5 | **N** | Everything: IPC, second resident process plus admin service | N |
| Ownership | Y | Y | Y | Licence-limited | Y |

**Why OS indexes cannot be the base (B).** The measurements in the Pulse survey show coverage gaps on both platforms. Changing that scope means touching user-owned system configuration (`mdutil` enablement, Windows Search scope) and triggering OS-wide indexing. We should not do that to make a name search work. Even where coverage exists, folder sizes, hard-link and clone accounting, excluded-directory control and event replay would still have to be ours. That makes B a layer on top of A, not an alternative.

**Does the gap list force D?** It forces our own catalogue, which is A′. It does not force D, because the part the OS gives is name search. In a catalogue that holds names for every entry anyway, that part costs about 12–20 B/entry (derived). Adding Spotlight or Windows Search for it would give us two result sets that disagree about coverage, and sizes that don't match search hits. Pulse's launcher already uses Spotlight for hub-chosen folders (`parity.md` H3). That remains an app-level choice, not a RightKit index dependency.

**Why not adopt C.**
- **FSearch, noahdunnagan (Rust, macOS, MIT).** The crawl, FSEvents binding and scorer are already in our crate with a notice (`THIRD-PARTY-NOTICES`). That is the same primitives (`getattrlistbulk` plus FSEvents), so the footprint gap cannot come from the primitives. Its README claims are self-measured and I did not verify them. Most likely its record carries less (no rollups, no 64-bit sizes, no hard-link or clone tables) and its update path takes the named item from the event. Both are things we can do ourselves.
- **FSearch, cboxdoerfer (C, Linux).** A different project, cited by the Pulse survey for design only: it queues inotify events and applies them once per second, updating the named entry. Its licence was not checked in either survey; I believe it is GPL, which I have not verified. It is not embeddable here.
- **Cardinal (macOS, Rust, Tauri).** An app, not a library, and its licence is unchecked. Its design (file-level FSEvents, covering-path reduction, stored event id) is a useful reference.
- **plocate.** Design reference only: names zstd-compressed in blocks, trigram posting lists, about 17 B/entry on disk, rescan that skips folders with unchanged ctime. It has no live tracking and no sizes.
- **Everything.** Closed source. Its SDK is IPC to a running Everything, so it means a second resident process and the same admin install. The Rust wrapper crate is GPL-3.0. Its design is the reference for Windows (MFT plus USN, about 100 B/entry).
- **Watchman.** Not an index of the disk. Its advice is useful: keep busy build folders out of the watch, and macOS filters only the first 8 ignore paths itself.
- Per the portfolio rule, an engine can be adopted only as a dependency or vendored design. None qualifies, and the vendored part is already in.

## 4. Recommendation: A′

### 4.1 Volume profile (Pulse), built on three changes

1. **Named-item upkeep.**
   - Use file-level FSEvents, with a latency of 1–3 s (Apple's guidance; Cardinal uses 0.1 s). For a changed item, `lstat` it and update that one entry; do not list its folder.
   - Re-listing remains only for `MustScanSubDirs`, `RootChanged`, mount events and newly seen directories, and then only that subtree. The 0.2.0 per-folder rate limiter stays as a governor for those cases. The Pulse survey found no source for per-folder backoff, so it is our invention and is not the main fix.
   - Drop events under excluded folders on a path-prefix check before any I/O. Because macOS filters only 8 ignore paths, the worker samples event rate per top-level folder and registers the 8 hottest as stream exclusions.
2. **Dropped events never mean a full rebuild.** Replace `full_rescan` with a root-level sweep that skips directories whose ctime is unchanged (plocate's technique), under the governor.
3. **Compact volume record.** Per-file columns shrink to roughly `name:u32 parent:u32 next:u32 kind/flags:u16 nlink:u16 mtime:u32 alloc:u32` (4 KiB units, overflow in a side table) `logical:u32`, about 28–32 B. `fileid`, `private`, `clone` and `dev` move to the existing SPECIAL side table, which covers only hard-linked and cloned files. Directories keep `first`/`prev`/`dir` and the 36 B rollup row.
   - Derived target: about 45–50 B/entry on disk including names, so about 1 GB for 20.7 M entries.
   - These are derived estimates, not measurements. They sit next to the Dell chat's unsourced 40–60 B resident estimate for Windows, Everything's stated 100 B resident (21 M ≈ 2 GB), and plocate's about 17 B on disk.

Lifecycle: build lazily on first Storage use, not at hub launch. Unload on idle, as Pulse's scan tier does at 120 s. Resume by stored event id; discard stale snapshots as `snapshot_max_age` already does. The README notes FSEvents history is finite and a too-old id gives no signal for the gap, so replay after a long idle needs the age check plus a ctime-skip sweep.

Whether Pulse needs a whole-disk index at all is a product question for Adrian and Pulse (§10). If the answer is "no", folder sizes stay with Pulse's scan tier and only the name-search tier is optional.

### 4.2 Hard-link and clone accounting, the OS cannot give it
- Keep ownership-by-first-link for hard links and group-wide maximum for clones. The latter is labelled `unique_is_estimate`.
- Spec §8's global-attribution-versus-per-folder question stays Adrian's. It does not change the representation above.

### 4.3 Windows
The ladder from the Dell chat is adopted, with two additions. It is rung-per-volume, best available:

1. **Elevated helper:** `FSCTL_ENUM_USN_DATA` plus `FSCTL_READ_USN_JOURNAL`; store journal id and next USN; full rescan on id change or lost range. The journal on the Dell is at most 32 MB, so wrap handling is mandatory.
2. **Unprivileged journal read:** only if it passes a Dell test. It is undocumented, names are stripped, and each changed id needs a lookup.
3. **Bulk walk (`FileIdBothDirectoryInfo`) plus `ReadDirectoryChangesW`:** what the hub does today. Lossy by design; rescan the subtree on overflow.
4. **Periodic rescan.**

Additions:
- **Rung 3 is the baseline.** Rung 1 is optional behind a feature flag, and it needs Adrian's decision on installing an admin-level helper. The spec's "unprivileged by default" invariant (§3) and Guard gating apply.
- **The Windows backend is built only after the Dell tests** (unprivileged journal read, non-system volume open as a normal user, bulk enumeration speed). Those tests need Adrian's decision on local builds on the Dell. MIT/Apache crates to read first: `usn-journal-rs` 0.4.1, `ntfs-reader` 0.6.0 (licences per the Pulse survey).
- **ReFS:** USN V3 records carry 128-bit ids. Record ids only as a side-table concern.

### 4.4 Workspace profile: deferred
It has no consumer on fsindex, and the one evidenced pain (the CodeRight picker re-walking per keystroke with a 10k cap) can be fixed with `rightkit-search`'s existing in-memory filename index fed by the existing walk. The spec's §7 feed, §9 policy views and §10 revocation machinery are large, driven by workspace consumers, and unrelated to the Pulse failure. They were the main driver of the RKFSIX03 growth to 108 B/entry. Revisit when a workspace consumer has a measured problem the current walkers cannot solve. The spec's §13.3 default becomes "per-consumer walkers".

## 5. Migration path from 0.1.x consumers

| Consumer | Today | Path |
|---|---|---|
| Pulse hub, Mac | `=0.1.0`, built at launch for every mount | Keep `PULSE_NO_DISK_INDEX=1` until a real-volume run of the new build meets §6. Then pin the new version, build lazily, unload on idle. `disk_index.rs` calls `search` and `sizes` only, so the API surface is small. Pulse keeps reclaim bounds, uncertainty and history. |
| Pulse hub, Windows | No index, plain scan | Unchanged until the Dell tests and the elevated-helper decision. |
| CodeRight `list_files`, Ledger, Blueprint | Do not use fsindex | No migration. Per-consumer walkers stay. |
| `rightkit-search::filename` | No users | Not retired; it is the picker's answer. |

Format: RKFSIX03 is not frozen for the volume profile; ledger #76 already says the format is not frozen against this decision. The first compact format gets a new magic. Older stores rebuild once, as the spec's upgrade rule already allows.

## 6. Runtime budget the recommendation commits to

Commitments for the volume profile, to be proved by the `budget` bench on Adrian's Mac at 20 M entries, then recorded in the README "Runtime budget" section per `docs/RUNTIME-BUDGETS.md`. Nothing below is measured yet.

- **On disk:** ≤ 60 B/entry (target 45–50).
- **Resident:** phys footprint ≤ 150 MB at rest at 20 M entries; transient search spike bounded and released by `trim`.
- **CPU:** ≤ 2% averaged over a quiet 5 minutes; ≤ 10% under build churn of 2k events/s.
- **Disk operations:** ≤ 200/s under that churn.
- **Governor:** if a bound is exceeded, shed (widen latency, drop the hottest folders, mark the index stale) rather than exceed it.
- **Initial crawl:** low priority and bounded threads. The 183 s at 500% figure is the number to beat or justify.
- **Exposure:** `status()` already carries resident bytes, re-list rate, directory reads and CPU. Add events dropped by prefix, governor state, and sweep counts.
- **Adoption gate:** the first whole-disk run on installed Pulse, with figures visible, before the hub ships it on.

## 7. What fsindex stops doing

- Stop freezing the 108 B RKFSIX03 layout and its Windows fields for the volume profile.
- Stop the Windows backend work (`FileIdExtd`, RDCW queues, USN probe, identity map) until the Dell tests and the helper decision.
- Stop the 0.3.0 workspace work: cursors, replay, barrier, policy views, listings.
- Stop treating per-folder backoff as the fix; keep it as the governor.
- Stop whole-store rebuilds on dropped events.
- Do not publish 0.2.0 as it stands. Ledger #76 already says it does not publish.

## 8. 0.2.0 lane: what to keep, change, stop

- **Keep:** pattern excludes and defaults (Cardinal also excludes caches and `/private/var`; nobody defaults `target`/`node_modules`, so those defaults are our own choice, backed by Watchman's advice); public `list_dir_fd` and `FsEventsWatcher` (ledger #61); the `Status` runtime figures; `shrink`; the `budget` bench.
- **Change:** file-level events with named-item update; ctime-skip sweep replacing `full_rescan`; the compact record; the lazy lifecycle; documented dynamic hot-folder stream exclusions.
- **Stop:** §7 above.

## 9. Open for Adrian

1. Does Pulse need a whole-disk name index, or only the folders the Storage page shows? This decides whether the volume catalogue is a feature or just an optional accelerator.
2. Elevated Windows helper: install an admin-level service, or ship rung 3 only?
3. Local builds on the Dell for the three open tests.
4. Rollup model (spec §8): global attribution or per-folder distinct accounting.
5. Spec §13.3: confirm workspace profile deferred.

## 10. Residual unverified items

- The noahdunnagan/fsearch README claims (not fetched).
- cboxdoerfer/fsearch and Cardinal licences (unchecked in the Pulse survey; my GPL belief for the former is unverified).
- How Spotlight consumes FSEvents; Everything's behaviour on journal wrap; the Everything SDK licence; stability of the unprivileged journal call across Windows versions.
- All byte-per-entry figures for the new layout (derived, not measured).
- What portion of the 5.4 GB was file-backed versus heap (no breakdown exists for the 0.1.0 run).

## 11. Correctness requirement (added by the RightKit lead, 2026-10-11)

Evidence supplied by Adrian after this adjudication (a comparison table, source unverified, likely from the fff side) shows the noahdunnagan/fsearch content search returning 58% to 86% of the matches fff returns on identical patterns (for example `dependencies` 2,942 against 5,041). That is content search, not name search, so it does not transfer directly to this index, but it means every fsearch speed and memory figure is unverified and that a performance figure without a correctness figure is worthless.

Binding on the recommendation in section 4 and the budget in section 6: every published performance figure carries a correctness figure measured in the same run, with the method stated:

- **Entry recall:** entries in the index against a ground-truth full walk of the same roots with the same exclusions; target 100%, any miss explained by a named exclusion.
- **Rollup accuracy:** folder sizes against a ground-truth walk; exact for logical and allocated, stated bound for `allocated_unique` where `unique_is_estimate` is set.
- **Change completeness under churn:** after a scripted burst (creates, renames, deletes at the churn rate in section 6) and a quiet period, the index equals a fresh walk; any shed work by the governor must leave the index marked stale, never silently wrong.

These figures go in the README "Runtime budget" section beside the memory and CPU figures and are part of the adoption gate.

