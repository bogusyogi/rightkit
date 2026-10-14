# RightKit status

Dated 2026-10-08. Basis: `git log` on `bogusyogi/claude` main through `948c9ee91`, the plan texts named below, and the files they cite. Commit SHAs are short forms. This file is the one place for "where is RightKit". Plans stay where they are; see `rightkit/docs/plans/README.md` for the index.

States: **done**, **in progress**, **not started**, **waiting on Adrian**, **blocked** (waits on another row).

## Headline

- The review plan (P0 to P4) is in progress. Code for P0 to P2 has landed on the Mac and Windows sides, but no P-row has its exit evidence recorded yet.
- Enforcement is not on. `rightkit/docs/OWNERSHIP-ENFORCEMENT.md:60` says installed enforcement is not active. No enforce commit exists.
- The Mac broker is the Developer-ID-signed RustRight `rustright-972495a`; the Windows XPS broker is `rustright-9115215`. The Node broker runtime is retired.
- Class-1 (tamper-proof evidence builds) is not installed on either host. Adrian must run the elevated installers.
- App chats stay stopped until Adrian resumes them.
- The broker is RustRight (`bogusyogi/rustright`), live on Mac and Windows; the Node broker packages were removed from RightKit on 2026-10-09. Only the RustRight chat edits broker code.

## (a) Review plan P0 to P4

Source: `tasks/reviews/2026-10-08-rightkit-converged-plan.md` (revision 4, both leads agree). Commits are on `bogusyogi/claude` main.

| Phase | Row | State | Commits | Open before done |
|---|---|---|---|---|
| P0 | Restore the Mac broker: control plane off external volume, root resolver, `EXTERNAL_ROOT_UNREADABLE` self-check, no direct-cargo fallback | in progress | `09dcd9839`, `085fedd00`, `fb28cca88` | Superseded: the signed RustRight broker (`rustright-972495a`) was cut over on Mac on 2026-10-09. Reboot and `LISTENING` evidence not recorded (`tasks/handoffs/2026-10-08-rustright-handoff.md`). |
| P0 | Restore the Mac broker: watchdog restarts a wedged broker | in progress | `5e00f3a57` | RustRight persists the watchdog restart cooldown. |
| P0 | Commit the catalog: revalidated tiered catalog | in progress | `09dcd9839`, `ce69ac34c` (catalog 3.0.0 trust candidate) | Complete-scope report and digest equality on both hosts not recorded. `ownership-report.mjs:8` scopes were hardcoded in the plan. |
| P0 | Commit the catalog: transitive lockfile skip (D3 stage 1) | in progress | `3ceb71e7a` | Stage 1 only. |
| P1 | Broker admission correctness (Windows) | in progress | `2b3da9e31`, `1e13ec279`, `ebd295302`, `5e00f3a57` | Refusal tests per D1 rejection list not recorded. Path-based owner exemption removal not evidenced. |
| P1 | Approval contracts and exception schema v2 (Windows) | in progress | `b23a212d6`, `ce69ac34c`, `0fd08a535`, `b5a1614fa` | Native stage/sign/verify on both hosts not recorded. |
| P1 | Publication binding (Mac): `publish-update` sealing, `.crate` byte binding | in progress | `5cce549c2`, `f1fc82fc0`, `25292c7e5`, `0ba65fa36` | Exit test (every entry rejects bad seals) not recorded. |
| P1 | D5 crate and package fixes | in progress | `be45aa01f` (secrets lock), `621bfcde2` (jobs, sandbox), `d5076a26f` (mcp, agent-room, hooks), `f1fc82fc0` (updates), `3937acf16` (env fallbacks), `a52f6d076` (db cipher gate) | `rightkit-pdf` `save.rs:414` gate and `sqlcipher-system` commit are open. |
| P1 | Mac enrollment and HeardRight exception drafts | waiting on Adrian | `b5a1614fa`, `064da06ba` | Regenerate drafts from a fresh scan after the P1 schema lands. Adrian signs. Do not sign before then. |
| P2 | Advisory gates: pre-commit gate, manifest and `cargo add`/`pnpm add` hook, catalog drift check | in progress | `065888365`, `6316a8491`, `5d3d1da2a` | Pre-commit gate in every app repo, keyword advisory, Legion `RightKit capabilities` block, `workspace-doctor` drift check: not recorded. |
| P2 | Full active-feature and production-reachability resolution | not started | (`3ceb71e7a` is stage 1 only) | Qualify before any enforce flip. |
| P2 | Enforce (block tier, both hosts) | not started | none | Blocked on P1 rows, complete registered-root coverage, validated exceptions, protected policy install, native acceptance. |
| P3 | Per-app burn-down | waiting on Adrian | see version pins below | App chats resume only when Adrian says so. |
| P4 | Cleanup: D6 retirements, `runtime-*` GC, `@rightkit/release` split | not started | `6fd70c759` (generated `INDEX.md`, partial) | The Node broker packages (including phase0 and staging) were removed on 2026-10-09; the broker is RustRight (`bogusyogi/rustright`). Remaining: `runtime-*` GC and the `@rightkit/release` split. |

P3 version pins (source versions in `Cargo.toml`, not yet published unless noted):

| Crate | Target | Source version | Commit |
|---|---|---|---|
| rightkit-db | 0.3 | 0.3.1 | `a52f6d076` |
| rightkit-shell | 0.1.3 | 0.1.4 | `473f64cb2` |
| rightkit-logs | 0.2 | 0.2.0 | not traced |
| rightkit-file-watch | 0.3.1 | 0.3.1 | `473f64cb2` |
| rightkit-oauth | 0.3 | 0.3.0 (server mode) | `74d34beed` |

## (b) Content pipeline RK0 to RK10

Source: `rightkit/docs/plans/2026-10-08-content-pipeline-execution-plan.md` (status table). Requirements: `docs/plans/2026-10-08-demo-recording-requirements.md`.

| Id | Package | State | Evidence |
|---|---|---|---|
| RK0 | Contracts (`rightkit/contracts/`, `rightkit-contracts`) | in progress | Built, commit `c5119392` (= `c51193924` in the log). Hetzner run waits on RK7. Publish waits on vendoring schemas. |
| RK1 | rightkit-oauth server mode, Linux secrets backend, Meta token exchange | blocked | Waits on RK7 and RK10. |
| RK2 A | Mac off-screen capture spike | in progress | Mac lead. |
| RK2 B | Windows WebView2 capture spike | done | Decision `3451a1f9`. Windows.Graphics.Capture on off-screen-visible window; hidden and minimized unsupported. Live run is RK3's first step. |
| RK3 | `rightkit-record` | blocked | Waits on RK0 and RK2 A. |
| RK4 | Alignment (paid and local routes) | in progress | Local route scaffolded (`rightkit-align`). Real QTS audio on Mac and XPS and the paid route are open. The paid route spends money and waits on Adrian's spend approval. |
| RK5 | Shared review capability | blocked | Waits on RK0. |
| RK6 | Channel memory schema | blocked | Waits on RK0. |
| RK7 | Linux native build route on Hetzner | in progress | Mac lead. |
| RK8 | Scenario runtime | blocked | Waits on RK0. |
| RK9 | Composition contract (`rightkit-compose`) | in progress | Model scaffolded (24 tests). Spring easing and click effects not modelled. |
| RK10 | Daemon host capability | blocked | Waits on RK0 and RK7. |

Plan rules that apply to every row: Rust at runtime (signed Swift capture helper excepted). Build through `rightkit cargo`. Hetzner builds through its own broker, never cross-compiled. Every new crate needs a catalog owner declaration before first publish.

## (c) Inventory

Scope: 50 crates, 29 npm packages, Swift package and helper, ownership-approval, contracts, scripts. Files in `rightkit/docs/audits/`.

| Step | State | Files |
|---|---|---|
| Evidence (Haiku dependency batches, npm, crosscutting; Luna crates, npm and Swift) | done | `2026-10-08-deps-batch1..4.md`, `-deps-npm.md`, `-inventory-haiku-crosscutting.md`, `-inventory-luna-crates.md`, `-inventory-luna-npm-swift-crosscutting.md`. Committed in `c51193924`. |
| Merge into one verified document | in progress | `2026-10-08-inventory-merged.md`, written 23:23, untracked, not committed. Section 6.5 lists items still unverifiable (Windows builds, RustSec data). |
| Fable and Astra review of the merged inventory | not started | No review file exists yet. |
| Fixes after review (removals, feature trims, merges, owner decisions in merged §6.4) | not started | Blocked on the review. |

## (d) Waits on Adrian

1. Resume app chats. They stay stopped until then (converged plan).
2. Run the elevated class-1 installers: macOS `_rightkit` LaunchDaemon and Windows service. Both scripts exist. Neither is installed (`8b8cecbf4`).
3. Grant the signed `rustright-broker` (Identifier `rustright-broker`, Team 6KLGD3LLKF) Removable Volumes access in System Settings > Privacy & Security > Files and Folders, so it can read the data drive. Class-1 evidence builds need this.
4. Done (2026-10-09): activation approved and applied via `rustright-install cutover --apply` on Mac and XPS. No action needed.
5. (Decided 2026-10-08) RustRight is a Rust rewrite of the broker; it replaced the Node broker after the parity suite (Mac matrix; partial app coverage, no full XPS matrix, V1 volume-loss deferred — see `docs/porting-plan.md` there), which was deleted from RightKit (`bogusyogi/rustright` docs/decisions/0001-rust-broker.md, `1a6a910`). The Node agent policy hooks (`packages/build-control-policy`) stay Node and live. Nothing to decide here.
6. Sign exception drafts for HeardRight, only after the P1 schema lands and drafts are regenerated.
7. Approve the paid route spend for RK4 (ElevenLabs), with per-run cost recorded.
8. Decide owner dispositions for zero-consumer crates and packages (merged inventory §6.4, for example `rightkit-computer` removal, `rightkit-pdf` adoption or stop publishing).

Already approved, no action needed: the Windows class-1 copy cost bound of 2 GiB and 20,000 files (`0b0ba220c`).

## (e) Ownership split

| Owner | Scope |
|---|---|
| RightKit Mac lead (this host) | Crate and npm publishing, ownership and enforcement system, inventory, content pipeline RK0, RK4, RK5, RK6, RK7, RK9, RK1, RK3, RK8, RK10 (the Mac parts), P0 Mac items, P1 publication binding, P2 Mac gate, P4 cleanup. |
| RightKit Windows lead (XPS, "Rightkit" session) | Windows class-1 pieces, P1 broker admission and approval contracts, `rightkit-credentials` `windows.rs`, Windows hooks, RK2 B (done), RK3 Windows helper. |
| RustRight (broker) | Owns the RustRight broker, `rustright` CLI (alias `rightkit`) and shims, installer `rustright-install` (the Node `build-control-{core,broker,clients,staging}` packages were deleted; `packages/build-control-policy` remains Node), `packages/release/managed-cargo.mjs`, `packages/release/managed-roots.mjs`. Only the RustRight chat edits these. Open broker list: `tasks/handoffs/2026-10-08-rustright-handoff.md` items 1 to 12. |

Rules for all three: one integration owner per repository. Only the owner changes HEAD, index, receipts or remote. Rust commands go through `rightkit cargo` (and `rustright cargo` once renamed). Change the live broker only through `rustright-install stage`, `approve`, `cutover --apply`; never hand-edit live config.

## How to update this file

1. Date the header. Replace the "Dated" line with the new date and the new `git log` basis commit.
2. For (a), change a row's state only when its exit evidence is recorded. Cite the commit SHA. Do not mark a P-row done on code landing alone.
3. For (b), copy the state table from the content-pipeline execution plan. Do not invent states there. Update the plan first, then this file.
4. For (c), update the merge and review rows from the file list in `rightkit/docs/audits/`.
5. For (d), remove items when Adrian decides them. Add new blocking decisions with the plan cite.
6. For (e), change ownership only when a plan or handoff changes it, and cite that document.
7. Keep the file under about 150 lines. Keep plans in their own files; link them instead of copying text.
8. Update `rightkit/docs/plans/README.md` when a plan is added, superseded or closed.
