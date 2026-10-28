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

