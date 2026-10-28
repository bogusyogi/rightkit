# Decisions

## 2026-10-28 14:34 IST — Consolidate gate requirements

- **Authorization:** Adrian said, “alright, go ahead with the consolidation please”.
- **Issue:** [rightkit#1254](https://github.com/bogusyogi/rightkit/issues/1254).
- **Decision:** `docs/GATE-CONTRACT.md` consolidates the three target requirements, nine-cell matrix, boundaries, acceptance evidence, & proposed phases pending Fable signoff. `docs/PIPELINE-ARCHITECTURE.md` identifies it as target state; existing architecture & tests remain current evidence until reviewed & reconciled.
- **Scope:** documentation only. No runtime success is claimed. All execution evidence remains NOT RUN. This consolidation does not authorize implementation, installation, delivery, or publication.

## 2026-10-28 — Correct gate-contract status after review

- **Source:** Fable review of `docs/GATE-CONTRACT.md` at local checkout HEAD `03c1111f`.
- **Decision:** Preserve current admission/authentication guards pending reviewed direct-origin trust design; qualify sealed origin output unchanged when it provides the required target, adding a native build only where origin cannot produce that target; keep private Linux qualification OPEN until a compatible Mac/Dell-hosted Linux environment is identified. Mark contract as target-state pending signoff; do not promote proposal text to current behavior.
- **Evidence:** [rightkit#1254](https://github.com/bogusyogi/rightkit/issues/1254), [rightkit#1241](https://github.com/bogusyogi/rightkit/issues/1241), [RustRight #174](https://github.com/bogusyogi/rustright/issues/174), [Pulse #492 acceptance comment](https://github.com/bogusyogi/pulse/issues/492#issuecomment-6466661659).
- **Scope:** Documentation correction only. All execution remains NOT RUN; this entry grants no additional implementation or publication authority.

## 2026-10-28 — Record execution hold & Linux-route reconciliation

- **Human hold:** Adrian said, “we will get to execution once fable signs off on the contract.” Fable reviews this contract; Fable is not production/publication authorizer. Sage/Astra adjudicate technical design, Oracle independently reviews, & Adrian retains reserved production, publication, & trust-policy decisions. No new approval is added for already-authorized routine work.
- **Requirement/source reconciliation:** The explicit user requirement says Linux has no broker. [rightkit#1253](https://github.com/bogusyogi/rightkit/issues/1253) contains an implementation proposal for Linux broker/config/pin; [comment 6467016714](https://github.com/bogusyogi/rightkit/issues/1253#issuecomment-6467016714) records the requirement. The proposal does not create a WSL broker exception or a new Adrian decision; implementation/closure is unverified.
- **Ownership:** [rightkit#1241](https://github.com/bogusyogi/rightkit/issues/1241) tracks the attested npm receipt gap; [RustRight #174](https://github.com/bogusyogi/rustright/issues/174) owns adapter argv/env/resource/receipt/native-proof work. RustRight #173 is broker contract; #91 is migration.
- **Scope:** Documentation only; execution remains NOT RUN.

## 2026-10-28 — Commit-only build; no gate on commit or push

- **Source:** Adrian in chat: “Why would push and commit need a gate? … The gate is for building.” and “we can never build through a dirty tree … committed is fine … as long as it's somewhere, as a record”.
- **Decision:** Commit and push run no gate and no local hook checks. The gate builds a snapshot of a selected commit from a gate-owned persistent source directory, with outputs in the existing build-output directory and the existing warm cache (separate source, not a second output or cache directory), every stage including `--to dev` and `--to test`; the primary checkout stays free for edits and uncommitted changes never enter a build; candidate identity is the commit SHA; warm cache is preserved by content sync (a fresh export per build is a defect); one cold build per repository per host is accepted (Adrian, 2026-10-28, on being told the cache cost). This replaces the proposed Class A input-binding blocker clause. No push is required by the gate. Adrian disputes the 2026-10-22 pre-push authorization recorded in `.rightkit-local-build.json`; that record needs correcting.
- **Evidence:** [rightkit#1254](https://github.com/bogusyogi/rightkit/issues/1254); `docs/GATE-CONTRACT.md` section 1.
- **Scope:** Documentation only; execution remains on hold and NOT RUN.

## 2026-10-28 — Gate contract locked; three interface gaps closed

- Adrian, RK chat: "discuss with RR, all good? both commit the docs, mark it as locked with my blessing." `docs/GATE-CONTRACT.md` is marked LOCKED and committed in this repository; `rustright/docs/BROKER-CONTRACT.md` is locked and committed by the RR chat (`fdd027b9`) in its own repository. Changes after the lock need Adrian's explicit reopen.
- Three interface gaps relayed by Adrian (verified against `packages/release/bin/rightkit-gate.mjs:1789-1797`, `packages/release/deterministic-scans.mjs:851`, `packages/release/advisory-db.mjs`) are closed in the gate contract's single interface definition: Linux resolves `target_directory` from the primary checkout and pins `CARGO_TARGET_DIR` for snapshot builds; Class A runs snapshot-aware (`check --commit <sha> --full-a --snapshot <dir>`, no HEAD/status read); verdict reuse re-runs `pnpm-audit` before acceptance (`refreshed`). Receipt gains `outputDir` & `refreshed`; acceptance rows E16 to E18. Approved by Adrian's relay; cross-checked with the RR chat.
- Next: a joint 12-step implementation plan, one step per validation gate; step N+1 starts only on Adrian's go after outside validation. Execution remains on hold until Adrian gives step 1 the go.


## 2026-10-29 — Joint plan step 1 accepted; step 2 dependency placement

- **Step 1 (gate-owned commit snapshot, rightkit#1255) accepted** by the coordinating Codex chat ("Review GATE-CONTRACT.md") at `5005deb56202f6782c7b960dcc3c20a4826a018d` (tree `a81f7541`), acting under Adrian's overnight instruction in that chat (2026-10-28 20:53 UTC, confirmed with `pulse chat said`): "once you approve and log the evidence, then step 2". Evidence ledger: `/Volumes/D/scratch/gate-contract-validation/VALIDATIONS.md`. Reviews: legion:oracle BLOCK at `aac073dd` and `a695c93e`, PASS at `899efb3d`, `23eff692`, `a64eb511`, `d2ea1b0a`, `5005deb5`; the coordinator's independent Oracle reviewed in parallel. The broker's verifier matched the gate and git on Mac and Dell at `a64eb511` (identity functions unchanged since). Fixtures Mac 23/23, Dell 17 pass with 6 POSIX-only skips. No contract edit.
- **Step 2 dependency placement: option A**, decided by the coordinator (not an Adrian quote) under the same instruction, for rightkit#1257. The code-executing Class A checks run in the snapshot with dependencies installed into it from the commit's lockfile: the pinned `packageManager`, `--offline --frozen-lockfile --ignore-scripts`, the host's existing absolute pnpm store resolved from the primary checkout's environment (Mac `/Volumes/D/.pnpm-store/v11`). No new store or cache, no network fallback, no execution of live checkout sources. The snapshot's `node_modules` is a lockfile-derived view that the next exact sync prunes. A store miss or a package that needs an install script is an actionable `RK_CHECK_ERROR` naming the root and package, never a skipped check. Setup cost is measured, not assumed.

## 2026-10-29 — Joint plan step 2 accepted (code and native-fixture scope); post-acceptance fixes

- **Step 2 (snapshot-aware Class A, live-scans, deterministic-red marker; rightkit#1257) accepted** by the coordinating Codex chat at `5450f01d0b2a92df6fa60c48adbb6f7728d68f05` (tree `3cd8dd83`), code and native-fixture scope only, under Adrian's overnight instruction in that chat. Ledger: `/Volumes/D/scratch/gate-contract-validation/VALIDATIONS.md`. Full native Cargo qualification stays PENDING joint step 4 (broker snapshot admission): on Mac and Dell the check fails closed at the managed-host Cargo rule. Eleven pre-existing Windows test failures are tracked in rightkit#1258.
- **Compiler-tool disposition (coordinator):** a skipped-build dependency that is itself a compile tool (esbuild) is not launched before Class A; its usability is deferred, not proven, and must be established by its first permitted post-Class-A invocation, where a failure stays a failure. Output labels it "deferred, not proven here".
- **Post-acceptance fixes** from legion:oracle's review of `79c4427b` (both still applied to `5450f01d`): a `pnpm-lockfile` failure (registry-dependent; `REGISTRY_SCAN_IDS`) never yields a deterministic marker; in snapshot mode on a broker host, cargo's workspace members in the primary checkout must equal the commit's (an untracked member or a stray directory matched by `crates/*` refuses before any cargo scan). Submitted to the coordinator for review; no contract change.
