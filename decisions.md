# Decisions

## 2026-10-28 14:34 IST — Consolidate gate requirements

- **Authorization:** Adrian said, “alright, go ahead with the consolidation please”.
- **Issue:** [rightkit#1254](https://github.com/bogusyogi/rightkit/issues/1254).
- **Decision:** `docs/GATE-CONTRACT.md` consolidates the three target requirements, nine-cell matrix, boundaries, acceptance evidence, & proposed phases pending Fable signoff. `docs/PIPELINE-ARCHITECTURE.md` identifies it as target state; existing architecture & tests remain current evidence until reviewed & reconciled.
- **Scope:** documentation only. No runtime success is claimed. All execution evidence remains NOT RUN. This consolidation does not authorize implementation, installation, delivery, or publication.

## 2026-10-28 — Correct gate-contract status after review

- **Source:** Fable review of `docs/GATE-CONTRACT.md` at checkout HEAD `af2c670eb51c`.
- **Decision:** Preserve current admission/authentication guards pending reviewed direct-origin trust design; qualify sealed origin output unchanged when it provides the required target, adding a native build only where origin cannot produce that target; keep private Linux qualification OPEN until a compatible Mac/Dell-hosted Linux environment is identified. Mark contract as target-state pending signoff; do not promote proposal text to current behavior.
- **Evidence:** [rightkit#1254](https://github.com/bogusyogi/rightkit/issues/1254), [RustRight #173](https://github.com/bogusyogi/rustright/issues/173), [RustRight #91](https://github.com/bogusyogi/rustright/issues/91), [Pulse #492 acceptance comment](https://github.com/bogusyogi/pulse/issues/492#issuecomment-6466661659).
- **Scope:** Documentation correction only. All execution remains NOT RUN; this entry grants no additional implementation or publication authority.
